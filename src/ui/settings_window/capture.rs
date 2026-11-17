//! The Privacy section's microphone and camera access rows: each device's current authorization
//! and the one action the system will accept.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Task, Window};
use spaceterm_ui::{
    Alert, AlertOutcome, ModalAction, ModalActionEmphasis, ModalActionIntent, ModalActionRole,
    ModalId,
};

use crate::platform::capture_access::{
    CaptureAccess, CaptureAccessError, CaptureAuthorization, CaptureDevice,
};
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use super::access_row::{
    self, AccessAction, AccessControl, AccessPresentation, AccessSelectors, CaptionKind,
};

/// The selectors and copy of one device's row.
pub(super) struct CaptureText {
    pub(super) row: AccessSelectors,
    pub(super) reset_modal: &'static str,
    pub(super) reset_confirm: &'static str,
    pub(super) reset_cancel: &'static str,
    purpose: &'static str,
    /// The device's name, which is also the Privacy & Security list that holds its grant.
    list: &'static str,
    /// The device in running text.
    noun: &'static str,
    restricted: &'static str,
    check_failed: &'static str,
    request_failed: &'static str,
}

pub(super) const MICROPHONE: CaptureText = CaptureText {
    row: AccessSelectors {
        control: "settings-microphone-access-control",
        status: "settings-microphone-access-status",
        allow: "settings-microphone-access-allow",
        open_settings: "settings-microphone-access-open-settings",
        cancel: "settings-microphone-access-cancel",
        check_again: "settings-microphone-access-check-again",
        progress: "settings-microphone-access-progress",
    },
    reset_modal: "settings-microphone-reset",
    reset_confirm: "settings-microphone-reset-confirm",
    reset_cancel: "settings-microphone-reset-cancel",
    purpose: "Lets terminal programs use the microphone.",
    list: "Microphone",
    noun: "microphone",
    restricted: "Your device\u{2019}s administrator has blocked microphone access.",
    check_failed: "Couldn\u{2019}t check microphone access.",
    request_failed: "Microphone access request failed.",
};

pub(super) const CAMERA: CaptureText = CaptureText {
    row: AccessSelectors {
        control: "settings-camera-access-control",
        status: "settings-camera-access-status",
        allow: "settings-camera-access-allow",
        open_settings: "settings-camera-access-open-settings",
        cancel: "settings-camera-access-cancel",
        check_again: "settings-camera-access-check-again",
        progress: "settings-camera-access-progress",
    },
    reset_modal: "settings-camera-reset",
    reset_confirm: "settings-camera-reset-confirm",
    reset_cancel: "settings-camera-reset-cancel",
    purpose: "Lets terminal programs use the camera.",
    list: "Camera",
    noun: "camera",
    restricted: "Your device\u{2019}s administrator has blocked camera access.",
    check_failed: "Couldn\u{2019}t check camera access.",
    request_failed: "Camera access request failed.",
};

/// The recovery after System Settings could not open. It names the entry the list shows: the
/// server bundle, named for this build's product.
fn open_failed(device: CaptureDevice) -> &'static str {
    static TEXT: [std::sync::OnceLock<String>; 2] =
        [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
    TEXT[device as usize].get_or_init(|| {
        format!(
            "Open System Settings and allow {} in Privacy & Security > {}.",
            crate::application_identity::ApplicationIdentity::current().display_name(),
            capture_text(device).list
        )
    })
}

pub(super) const fn capture_text(device: CaptureDevice) -> &'static CaptureText {
    match device {
        CaptureDevice::Microphone => &MICROPHONE,
        CaptureDevice::Camera => &CAMERA,
    }
}

/// What SpaceTerm currently knows about one device's access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureAccessStatus {
    /// No capability is composed, so access is neither readable nor changeable here.
    Unsupported,
    Authorization(CaptureAuthorization),
    /// One explicit request awaits the system's decision.
    Requesting,
    /// Authorization could not be read or requested.
    Failed(CaptureAccessError),
}

/// Where the latest reset of the device's decision stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureReset {
    Running,
    Failed,
}

/// The reset confirmation's decisions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResetDecision {
    Reset,
    Cancel,
}

/// Owns one device's injected capability, its last known status, and the pending native bridges.
pub(super) struct CaptureAccessRow {
    device: CaptureDevice,
    access: Option<Rc<dyn CaptureAccess>>,
    pub(super) status: CaptureAccessStatus,
    /// A recovery that could not open the system's settings, kept beside the denial it meant to fix.
    recovery_failure: Option<CaptureAccessError>,
    /// The latest reset, kept beside the decision it meant to clear.
    pub(super) reset: Option<CaptureReset>,
    /// Returns the system's decision to GPUI. Dropping the window drops the bridge, and a late
    /// decision then has nowhere to land.
    _request: Option<Task<()>>,
    /// Returns the reset's result to GPUI.
    _reset: Option<Task<()>>,
}

impl CaptureAccessRow {
    pub(super) fn new(device: CaptureDevice, access: Option<Rc<dyn CaptureAccess>>) -> Self {
        let mut row = Self {
            device,
            access,
            status: CaptureAccessStatus::Unsupported,
            recovery_failure: None,
            reset: None,
            _request: None,
            _reset: None,
        };
        row.refresh();
        row
    }

    /// Reads the current authorization. A pending request keeps its status until the system
    /// answers, so a window activation the prompt causes cannot race the decision.
    pub(super) fn refresh(&mut self) {
        if self.status != CaptureAccessStatus::Requesting {
            self.apply(self.read());
        }
    }

    /// Applies the system's decision, or reads authorization again when none was delivered.
    fn finish_request(&mut self, decision: Option<CaptureAuthorization>) {
        let status = decision.map_or_else(|| self.read(), CaptureAccessStatus::Authorization);
        self.apply(status);
    }

    fn read(&self) -> CaptureAccessStatus {
        match &self.access {
            None => CaptureAccessStatus::Unsupported,
            Some(access) => match access.authorization(self.device) {
                Ok(authorization) => CaptureAccessStatus::Authorization(authorization),
                Err(error) => CaptureAccessStatus::Failed(error),
            },
        }
    }

    /// A recovery or reset failure belongs to the status it tried to change, so any other status
    /// retires it. A running reset keeps its place until it reports.
    fn apply(&mut self, status: CaptureAccessStatus) {
        if status != self.status && self.reset == Some(CaptureReset::Failed) {
            self.reset = None;
        }
        self.status = status;
        if status != CaptureAccessStatus::Authorization(CaptureAuthorization::Denied) {
            self.recovery_failure = None;
        }
    }

    fn open_settings(&mut self) {
        if self.status != CaptureAccessStatus::Authorization(CaptureAuthorization::Denied) {
            return;
        }
        let Some(access) = &self.access else {
            return;
        };
        self.recovery_failure = access.open_settings(self.device).err();
    }

    /// Whether the device holds a decision the reset can clear right now.
    fn can_reset(&self) -> bool {
        matches!(
            self.status,
            CaptureAccessStatus::Authorization(
                CaptureAuthorization::Authorized | CaptureAuthorization::Denied
            )
        ) && self.reset != Some(CaptureReset::Running)
            && self
                .access
                .as_ref()
                .is_some_and(|access| access.can_reset())
    }

    /// Applies a reset's result. The capability verifies before it reports, so the read here
    /// already reflects the reset.
    fn finish_reset(&mut self, result: Option<Result<(), CaptureAccessError>>) {
        self.reset = None;
        self.refresh();
        if !matches!(result, Some(Ok(()))) {
            self.reset = Some(CaptureReset::Failed);
        }
    }

    pub(super) fn presentation(&self) -> AccessPresentation {
        use CaptureAuthorization as Authorization;

        let text = capture_text(self.device);
        let status = |label| AccessControl::Status {
            label,
            allowed: false,
        };
        let guidance = |caption: &'static str| (CaptionKind::Guidance, caption.to_owned());
        let reset_failure = (self.reset == Some(CaptureReset::Failed)).then(|| {
            (
                CaptionKind::Error,
                format!("Couldn\u{2019}t reset {}. Try again.", text.list),
            )
        });
        let (control, (caption_kind, caption)) = match self.status {
            CaptureAccessStatus::Unsupported => (
                status("Unavailable"),
                guidance("Unavailable on this platform."),
            ),
            // A running reset owns the row until it reports, whatever the reads in between say.
            CaptureAccessStatus::Authorization(_) if self.reset == Some(CaptureReset::Running) => (
                status("Waiting"),
                (
                    CaptionKind::Waiting,
                    format!("Resetting {}\u{2026}", text.list),
                ),
            ),
            CaptureAccessStatus::Authorization(Authorization::Authorized) => (
                AccessControl::Status {
                    label: "Allowed",
                    allowed: true,
                },
                reset_failure.unwrap_or_else(|| guidance(text.purpose)),
            ),
            CaptureAccessStatus::Authorization(Authorization::NotDetermined) => (
                AccessControl::Action(AccessAction::Allow),
                guidance(text.purpose),
            ),
            CaptureAccessStatus::Requesting => (
                status("Waiting"),
                (
                    CaptionKind::Waiting,
                    "Respond to the system prompt.".to_owned(),
                ),
            ),
            CaptureAccessStatus::Authorization(Authorization::Denied) => (
                AccessControl::Action(AccessAction::OpenSettings),
                match self.recovery_failure {
                    Some(_) => (CaptionKind::Error, open_failed(self.device).to_owned()),
                    None => reset_failure.unwrap_or_else(|| guidance(text.purpose)),
                },
            ),
            CaptureAccessStatus::Authorization(Authorization::Restricted) => {
                (status("Restricted"), guidance(text.restricted))
            }
            CaptureAccessStatus::Failed(error) => (
                AccessControl::Action(AccessAction::CheckAgain),
                (
                    CaptionKind::Error,
                    match error {
                        CaptureAccessError::PlatformUnavailable => text.check_failed,
                        CaptureAccessError::PlatformRejected => text.request_failed,
                    }
                    .to_owned(),
                ),
            ),
        };
        AccessPresentation {
            control,
            caption: caption.into(),
            caption_kind,
            resettable: self.can_reset(),
        }
    }
}

impl SettingsWindow {
    pub(super) fn capture_row(&mut self, device: CaptureDevice) -> &mut CaptureAccessRow {
        match device {
            CaptureDevice::Microphone => &mut self.microphone_access,
            CaptureDevice::Camera => &mut self.camera_access,
        }
    }

    /// Asks the system for one device's access, once, from the Not Determined state. The native
    /// completion may run on any queue, so it sends the decision through a channel.
    pub(super) fn request_capture_access(&mut self, device: CaptureDevice, cx: &mut Context<Self>) {
        let row = self.capture_row(device);
        if row.status != CaptureAccessStatus::Authorization(CaptureAuthorization::NotDetermined) {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        match access.request_authorization(
            device,
            Box::new(move |decision| {
                let _ = sender.try_send(decision);
            }),
        ) {
            Ok(()) => {
                row.status = CaptureAccessStatus::Requesting;
                row._request = Some(cx.spawn(async move |settings, cx| {
                    let decision = receiver.recv().await.ok();
                    let _ = settings.update(cx, |settings, cx| {
                        settings.capture_row(device).finish_request(decision);
                        cx.notify();
                    });
                }));
            }
            Err(error) => row.status = CaptureAccessStatus::Failed(error),
        }
        cx.notify();
    }

    pub(super) fn open_capture_settings(&mut self, device: CaptureDevice, cx: &mut Context<Self>) {
        self.capture_row(device).open_settings();
        cx.notify();
    }

    /// Reads both devices' authorization again.
    pub(super) fn refresh_capture_access(&mut self, cx: &mut Context<Self>) {
        self.microphone_access.refresh();
        self.camera_access.refresh();
        cx.notify();
    }

    fn refresh_capture_row(&mut self, device: CaptureDevice, cx: &mut Context<Self>) {
        self.capture_row(device).refresh();
        cx.notify();
    }

    /// Asks before resetting the device's decision, because the reset removes an allowed grant.
    pub(super) fn confirm_capture_reset(
        &mut self,
        device: CaptureDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.capture_row(device).can_reset() {
            return;
        }
        let text = capture_text(device);
        let title = format!("Reset {}?", text.list);
        let owner = cx.weak_entity();
        let result = Alert::new(
            ModalId::new(text.reset_modal),
            title.clone(),
            title,
            format!(
                "Terminal programs lose {} access. Choose Allow\u{2026} afterward and the system asks again.",
                text.noun
            ),
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
        .present(window, cx, move |outcome, cx| {
            let AlertOutcome::Activated {
                action_id: ResetDecision::Reset,
                ..
            } = outcome
            else {
                return;
            };
            let _ = owner.update(cx, |settings, cx| settings.reset_capture(device, cx));
        });
        if result.is_err() {
            eprintln!("failed to present the SpaceTerm capture reset alert");
        }
    }

    /// Clears the device's decision, so the row offers Allow and the system prompts again. The
    /// native completion may run on any thread, so it sends the result through a channel.
    pub(super) fn reset_capture(&mut self, device: CaptureDevice, cx: &mut Context<Self>) {
        let row = self.capture_row(device);
        if !row.can_reset() {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        match access.reset(
            device,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        ) {
            Ok(()) => {
                row.reset = Some(CaptureReset::Running);
                row._reset = Some(cx.spawn(async move |settings, cx| {
                    let result = receiver.recv().await.ok();
                    let _ = settings.update(cx, |settings, cx| {
                        settings.capture_row(device).finish_reset(result);
                        cx.notify();
                    });
                }));
            }
            Err(_) => row.reset = Some(CaptureReset::Failed),
        }
        cx.notify();
    }

    /// The row's one trailing control: its next step, or its state.
    pub(super) fn render_capture_access(
        &mut self,
        device: CaptureDevice,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let control = self.capture_row(device).presentation().control;
        let owner = cx.weak_entity();
        access_row::render_control(
            &capture_text(device).row,
            control,
            appearance,
            move |action, _, cx| {
                let _ = owner.update(cx, |settings, cx| match action {
                    AccessAction::Allow => settings.request_capture_access(device, cx),
                    AccessAction::OpenSettings => settings.open_capture_settings(device, cx),
                    AccessAction::CheckAgain => settings.refresh_capture_row(device, cx),
                    AccessAction::Cancel => {}
                });
            },
        )
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
