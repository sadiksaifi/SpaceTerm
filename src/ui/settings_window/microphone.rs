//! The Privacy section's microphone access row: the current authorization and its one honest action.
//!
//! Voice tools running in a Terminal Session inherit SpaceTerm's microphone authorization, so this
//! row is where a person learns why such a tool cannot hear them and how to fix it. The row never
//! offers an action the system would refuse: it requests authorization only while the system has
//! not decided, sends a denied decision to the system's privacy settings, and explains a restriction
//! without pretending it can prompt.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Task, div};

use crate::platform::microphone_access::{
    MicrophoneAccess, MicrophoneAccessError, MicrophoneAuthorization,
};
use crate::ui::appearance::ChromeAppearance;

use super::SettingsWindow;
use super::controls::{action_button, badge};

pub(super) const CONTROL_SELECTOR: &str = "settings-microphone-access-control";
pub(super) const REQUEST_SELECTOR: &str = "settings-microphone-access-request";
pub(super) const OPEN_SETTINGS_SELECTOR: &str = "settings-microphone-access-open-settings";
pub(super) const CHECK_AGAIN_SELECTOR: &str = "settings-microphone-access-check-again";

/// What SpaceTerm currently knows about microphone access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MicrophoneAccessStatus {
    /// No capability is composed, so access is neither readable nor changeable here.
    Unsupported,
    Authorization(MicrophoneAuthorization),
    /// One explicit request awaits the system's decision.
    Requesting,
    /// Authorization could not be read or requested.
    Failed(MicrophoneAccessError),
}

/// The action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MicrophoneAccessAction {
    Request,
    /// The request button, withheld while the system prompt is answered.
    Requesting,
    OpenSettings,
    CheckAgain,
}

/// The row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct MicrophoneAccessPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: &'static str,
    pub(super) action: Option<MicrophoneAccessAction>,
}

/// Owns the injected capability, the last known status, and the pending request bridge.
pub(super) struct MicrophoneAccessRow {
    access: Option<Rc<dyn MicrophoneAccess>>,
    status: MicrophoneAccessStatus,
    /// A recovery that could not open the system's settings, kept beside the denial it meant to fix.
    recovery_failure: Option<MicrophoneAccessError>,
    /// Returns the native decision to GPUI. Dropping the window drops the bridge, and a late
    /// decision then has nowhere to land.
    _request: Option<Task<()>>,
}

impl MicrophoneAccessRow {
    pub(super) fn new(access: Option<Rc<dyn MicrophoneAccess>>) -> Self {
        let mut row = Self {
            access,
            status: MicrophoneAccessStatus::Unsupported,
            recovery_failure: None,
            _request: None,
        };
        row.refresh();
        row
    }

    #[cfg(test)]
    pub(super) fn status(&self) -> MicrophoneAccessStatus {
        self.status
    }

    /// Reads the current authorization, which can change in the system's settings at any time.
    ///
    /// A pending request keeps its status until the system answers it, so a window activation
    /// caused by the prompt itself cannot race the decision.
    pub(super) fn refresh(&mut self) {
        if self.status != MicrophoneAccessStatus::Requesting {
            self.apply(self.read());
        }
    }

    /// Applies the system's decision, or reads authorization again when none was delivered.
    fn finish_request(&mut self, decision: Option<MicrophoneAuthorization>) {
        let status = decision.map_or_else(|| self.read(), MicrophoneAccessStatus::Authorization);
        self.apply(status);
    }

    fn read(&self) -> MicrophoneAccessStatus {
        match &self.access {
            None => MicrophoneAccessStatus::Unsupported,
            Some(access) => match access.authorization() {
                Ok(authorization) => MicrophoneAccessStatus::Authorization(authorization),
                Err(error) => MicrophoneAccessStatus::Failed(error),
            },
        }
    }

    /// A recovery failure belongs to the denial it tried to fix, so any other status retires it.
    fn apply(&mut self, status: MicrophoneAccessStatus) {
        self.status = status;
        if status != MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Denied) {
            self.recovery_failure = None;
        }
    }

    fn open_settings(&mut self) {
        if self.status != MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::Denied) {
            return;
        }
        let Some(access) = &self.access else {
            return;
        };
        self.recovery_failure = access.open_settings().err();
    }

    pub(super) fn presentation(&self) -> MicrophoneAccessPresentation {
        use MicrophoneAccessAction as Action;
        use MicrophoneAuthorization as Authorization;

        let (state, state_selector, explanation, action) = match self.status {
            MicrophoneAccessStatus::Unsupported => (
                "Unavailable",
                "settings-microphone-access-state-unavailable",
                "SpaceTerm does not manage microphone access on this platform.",
                None,
            ),
            MicrophoneAccessStatus::Authorization(Authorization::Authorized) => (
                "Allowed",
                "settings-microphone-access-state-allowed",
                "Voice tools running in SpaceTerm can use the microphone.",
                None,
            ),
            MicrophoneAccessStatus::Authorization(Authorization::NotDetermined) => (
                "Not Requested",
                "settings-microphone-access-state-not-requested",
                "Voice tools running in SpaceTerm need permission to use the microphone. The \
                 system asks once.",
                Some(Action::Request),
            ),
            MicrophoneAccessStatus::Requesting => (
                "Requesting",
                "settings-microphone-access-state-requesting",
                "Allow or deny microphone access in the system prompt.",
                Some(Action::Requesting),
            ),
            MicrophoneAccessStatus::Authorization(Authorization::Denied) => (
                "Denied",
                "settings-microphone-access-state-denied",
                match self.recovery_failure {
                    None => {
                        "Voice tools running in SpaceTerm cannot use the microphone. Allow \
                         SpaceTerm in System Settings, then try the tool again."
                    }
                    Some(_) => {
                        "System Settings could not be opened. Allow SpaceTerm under Privacy & \
                         Security > Microphone, then try the tool again."
                    }
                },
                Some(Action::OpenSettings),
            ),
            MicrophoneAccessStatus::Authorization(Authorization::Restricted) => (
                "Restricted",
                "settings-microphone-access-state-restricted",
                "A system policy, such as device management, prevents microphone access. \
                 SpaceTerm cannot change it.",
                None,
            ),
            MicrophoneAccessStatus::Failed(error) => (
                "Unavailable",
                "settings-microphone-access-state-unavailable",
                match error {
                    MicrophoneAccessError::OffMainThread => {
                        "SpaceTerm could not check microphone access."
                    }
                    MicrophoneAccessError::PlatformUnavailable => {
                        "The system did not report microphone access."
                    }
                    MicrophoneAccessError::PlatformRejected => {
                        "The system rejected the microphone access request."
                    }
                },
                Some(Action::CheckAgain),
            ),
        };
        MicrophoneAccessPresentation {
            state,
            state_selector,
            explanation,
            action,
        }
    }
}

impl SettingsWindow {
    /// Asks the system for microphone access, once, from the Not Determined state.
    ///
    /// The native completion may run on any queue. It only sends the closed decision through a
    /// channel, and the window applies it on the foreground executor before repainting.
    pub(super) fn request_microphone_access(&mut self, cx: &mut Context<Self>) {
        let row = &mut self.microphone_access;
        if row.status
            != MicrophoneAccessStatus::Authorization(MicrophoneAuthorization::NotDetermined)
        {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        match access.request_authorization(Box::new(move |decision| {
            let _ = sender.try_send(decision);
        })) {
            Ok(()) => {
                row.status = MicrophoneAccessStatus::Requesting;
                row._request = Some(cx.spawn(async move |settings, cx| {
                    let decision = receiver.recv().await.ok();
                    let _ = settings.update(cx, |settings, cx| {
                        settings.microphone_access.finish_request(decision);
                        cx.notify();
                    });
                }));
            }
            Err(error) => row.status = MicrophoneAccessStatus::Failed(error),
        }
        cx.notify();
    }

    pub(super) fn open_microphone_settings(&mut self, cx: &mut Context<Self>) {
        self.microphone_access.open_settings();
        cx.notify();
    }

    pub(super) fn refresh_microphone_access(&mut self, cx: &mut Context<Self>) {
        self.microphone_access.refresh();
        cx.notify();
    }

    /// The state badge and, when one applies, the action beside it.
    pub(super) fn render_microphone_access(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let presentation = self.microphone_access.presentation();
        let state_selector = presentation.state_selector;
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| {
            let owner = owner.clone();
            match action {
                MicrophoneAccessAction::Request | MicrophoneAccessAction::Requesting => {
                    action_button(
                        REQUEST_SELECTOR,
                        "Request Access…",
                        action == MicrophoneAccessAction::Request,
                        move |_, cx| {
                            let _ = owner
                                .update(cx, |settings, cx| settings.request_microphone_access(cx));
                        },
                    )
                }
                MicrophoneAccessAction::OpenSettings => action_button(
                    OPEN_SETTINGS_SELECTOR,
                    "Open System Settings",
                    true,
                    move |_, cx| {
                        let _ =
                            owner.update(cx, |settings, cx| settings.open_microphone_settings(cx));
                    },
                ),
                MicrophoneAccessAction::CheckAgain => {
                    action_button(CHECK_AGAIN_SELECTOR, "Check Again", true, move |_, cx| {
                        let _ =
                            owner.update(cx, |settings, cx| settings.refresh_microphone_access(cx));
                    })
                }
            }
        });
        div()
            .debug_selector(|| CONTROL_SELECTOR.to_owned())
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
