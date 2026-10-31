//! The Privacy section's microphone and camera access rows: each device's current authorization
//! and the one action the system will accept.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Task, div};

use crate::platform::capture_access::{
    CaptureAccess, CaptureAccessError, CaptureAuthorization, CaptureDevice,
};
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use crate::ui::sidebar_window::form::{action_button, badge};

/// The selectors and copy of one device's row.
pub(super) struct CaptureText {
    pub(super) control: &'static str,
    pub(super) request: &'static str,
    pub(super) open_settings: &'static str,
    pub(super) check_again: &'static str,
    pub(super) state_unavailable: &'static str,
    pub(super) state_allowed: &'static str,
    pub(super) state_not_requested: &'static str,
    pub(super) state_requesting: &'static str,
    pub(super) state_denied: &'static str,
    pub(super) state_restricted: &'static str,
    purpose: &'static str,
    /// The Privacy & Security list that holds the device's grant.
    list: &'static str,
    restricted: &'static str,
    check_failed: &'static str,
    request_failed: &'static str,
}

pub(super) const MICROPHONE: CaptureText = CaptureText {
    control: "settings-microphone-access-control",
    request: "settings-microphone-access-request",
    open_settings: "settings-microphone-access-open-settings",
    check_again: "settings-microphone-access-check-again",
    state_unavailable: "settings-microphone-access-state-unavailable",
    state_allowed: "settings-microphone-access-state-allowed",
    state_not_requested: "settings-microphone-access-state-not-requested",
    state_requesting: "settings-microphone-access-state-requesting",
    state_denied: "settings-microphone-access-state-denied",
    state_restricted: "settings-microphone-access-state-restricted",
    purpose: "Lets terminal programs use the microphone.",
    list: "Microphone",
    restricted: "Your device\u{2019}s administrator has blocked microphone access.",
    check_failed: "Couldn\u{2019}t check microphone access.",
    request_failed: "Microphone access request failed.",
};

pub(super) const CAMERA: CaptureText = CaptureText {
    control: "settings-camera-access-control",
    request: "settings-camera-access-request",
    open_settings: "settings-camera-access-open-settings",
    check_again: "settings-camera-access-check-again",
    state_unavailable: "settings-camera-access-state-unavailable",
    state_allowed: "settings-camera-access-state-allowed",
    state_not_requested: "settings-camera-access-state-not-requested",
    state_requesting: "settings-camera-access-state-requesting",
    state_denied: "settings-camera-access-state-denied",
    state_restricted: "settings-camera-access-state-restricted",
    purpose: "Lets terminal programs use the camera.",
    list: "Camera",
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

/// The action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CaptureAccessAction {
    Request,
    /// The request button, withheld while the system prompt is answered.
    Requesting,
    OpenSettings,
    CheckAgain,
}

/// The row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CaptureAccessPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: &'static str,
    pub(super) action: Option<CaptureAccessAction>,
}

/// Owns one device's injected capability, its last known status, and the pending request bridge.
pub(super) struct CaptureAccessRow {
    device: CaptureDevice,
    access: Option<Rc<dyn CaptureAccess>>,
    status: CaptureAccessStatus,
    /// A recovery that could not open the system's settings, kept beside the denial it meant to fix.
    recovery_failure: Option<CaptureAccessError>,
    /// Returns the system's decision to GPUI. Dropping the window drops the bridge, and a late
    /// decision then has nowhere to land.
    _request: Option<Task<()>>,
}

impl CaptureAccessRow {
    pub(super) fn new(device: CaptureDevice, access: Option<Rc<dyn CaptureAccess>>) -> Self {
        let mut row = Self {
            device,
            access,
            status: CaptureAccessStatus::Unsupported,
            recovery_failure: None,
            _request: None,
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

    /// A recovery failure belongs to the denial it tried to fix, so any other status retires it.
    fn apply(&mut self, status: CaptureAccessStatus) {
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

    pub(super) fn presentation(&self) -> CaptureAccessPresentation {
        use CaptureAccessAction as Action;
        use CaptureAuthorization as Authorization;

        let text = capture_text(self.device);
        let (state, state_selector, explanation, action) = match self.status {
            CaptureAccessStatus::Unsupported => (
                "Unavailable",
                text.state_unavailable,
                "Unavailable on this platform.",
                None,
            ),
            CaptureAccessStatus::Authorization(Authorization::Authorized) => {
                ("Allowed", text.state_allowed, text.purpose, None)
            }
            CaptureAccessStatus::Authorization(Authorization::NotDetermined) => (
                "Not requested",
                text.state_not_requested,
                text.purpose,
                Some(Action::Request),
            ),
            CaptureAccessStatus::Requesting => (
                "Requesting",
                text.state_requesting,
                "Respond to the system prompt.",
                Some(Action::Requesting),
            ),
            CaptureAccessStatus::Authorization(Authorization::Denied) => (
                "Denied",
                text.state_denied,
                match self.recovery_failure {
                    None => text.purpose,
                    Some(_) => open_failed(self.device),
                },
                Some(Action::OpenSettings),
            ),
            CaptureAccessStatus::Authorization(Authorization::Restricted) => {
                ("Restricted", text.state_restricted, text.restricted, None)
            }
            CaptureAccessStatus::Failed(error) => (
                "Unavailable",
                text.state_unavailable,
                match error {
                    CaptureAccessError::PlatformUnavailable => text.check_failed,
                    CaptureAccessError::PlatformRejected => text.request_failed,
                },
                Some(Action::CheckAgain),
            ),
        };
        CaptureAccessPresentation {
            state,
            state_selector,
            explanation,
            action,
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

    /// The state badge and, when one applies, the action beside it.
    pub(super) fn render_capture_access(
        &mut self,
        device: CaptureDevice,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let text = capture_text(device);
        let presentation = self.capture_row(device).presentation();
        let state_selector = presentation.state_selector;
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| {
            let owner = owner.clone();
            match action {
                CaptureAccessAction::Request | CaptureAccessAction::Requesting => action_button(
                    text.request,
                    "Request Access…",
                    action == CaptureAccessAction::Request,
                    move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.request_capture_access(device, cx);
                        });
                    },
                ),
                CaptureAccessAction::OpenSettings => action_button(
                    text.open_settings,
                    "Open System Settings",
                    true,
                    move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.open_capture_settings(device, cx);
                        });
                    },
                ),
                CaptureAccessAction::CheckAgain => {
                    action_button(text.check_again, "Check Again", true, move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.refresh_capture_row(device, cx);
                        });
                    })
                }
            }
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
                    .child(badge(state_selector, presentation.state, appearance)),
            )
            .children(action)
            .into_any_element()
    }
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
