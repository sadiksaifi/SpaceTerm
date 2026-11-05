//! The Privacy section's Notifications rows: the desktop's own notification authorization with
//! the one action the system will accept, and the Client Setting for terminal attention
//! notifications. ADR 0063 records why the desktop, not SpaceTerm Core, owns this grant.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{AnyElement, Task, div};
use spaceterm_ui::{Switch, ToggleSize};

use crate::platform::notification_access::{
    NotificationAccess, NotificationAccessError, NotificationAccessState,
};
use crate::settings::notifications::NotificationPreferences;
use crate::ui::appearance::ChromeAppearance;
use crate::ui::sidebar_window::form::{action_button, badge};

use super::{SettingsRowId, SettingsWindow};

pub(super) const CONTROL: &str = "settings-notifications-access-control";
pub(super) const REQUEST: &str = "settings-notifications-access-request";
pub(super) const OPEN_SETTINGS: &str = "settings-notifications-access-open-settings";
pub(super) const CHECK_AGAIN: &str = "settings-notifications-access-check-again";
pub(super) const TERMINAL_ATTENTION: &str = "settings-terminal-attention-notifications";

const PURPOSE: &str = "SpaceTerm notifies you when a terminal needs attention or an update is due.";

/// The recovery after System Settings could not open. It names the entry the list shows: this
/// build's desktop.
fn open_failed() -> &'static str {
    static TEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TEXT.get_or_init(|| {
        format!(
            "Open System Settings and allow {} in Notifications.",
            crate::application_identity::ApplicationIdentity::current().display_name()
        )
    })
}

/// What SpaceTerm currently knows about its notification authorization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationAccessStatus {
    /// No capability is composed, so authorization is neither readable nor changeable here.
    Unsupported,
    /// The first read has not answered yet.
    Checking,
    State(NotificationAccessState),
    /// One explicit request awaits the person's answer to the system prompt.
    Requesting,
    /// Authorization could not be read or requested.
    Failed(NotificationAccessError),
}

/// The action the row offers for its status, when there is one worth offering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationAccessAction {
    Request,
    /// The request button, withheld while the system prompt is answered.
    Requesting,
    OpenSettings,
    CheckAgain,
}

/// The row's complete presentation, derived from status alone so every state reads one way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct NotificationAccessPresentation {
    pub(super) state: &'static str,
    /// Names the state badge, so a test can tell each state apart by geometry.
    pub(super) state_selector: &'static str,
    pub(super) explanation: &'static str,
    pub(super) action: Option<NotificationAccessAction>,
}

/// Owns the injected capability, the last known status, and the bridges that return native
/// answers to GPUI. Dropping the window drops the bridges, and a late answer has nowhere to land.
pub(super) struct NotificationAccessRow {
    access: Option<Rc<dyn NotificationAccess>>,
    pub(super) status: NotificationAccessStatus,
    /// A recovery that could not open the system's settings, kept beside the state it meant to fix.
    recovery_failure: Option<NotificationAccessError>,
    _read: Option<Task<()>>,
    _request: Option<Task<()>>,
}

impl NotificationAccessRow {
    pub(super) fn new(access: Option<Rc<dyn NotificationAccess>>) -> Self {
        Self {
            status: if access.is_some() {
                NotificationAccessStatus::Checking
            } else {
                NotificationAccessStatus::Unsupported
            },
            access,
            recovery_failure: None,
            _read: None,
            _request: None,
        }
    }

    /// A recovery failure belongs to the state it tried to fix, so any other status retires it.
    fn apply(&mut self, status: NotificationAccessStatus) {
        if status != self.status {
            self.recovery_failure = None;
        }
        self.status = status;
    }

    fn opens_settings(&self) -> bool {
        matches!(
            self.status,
            NotificationAccessStatus::State(
                NotificationAccessState::Quiet
                    | NotificationAccessState::Hidden
                    | NotificationAccessState::Denied
            )
        )
    }

    pub(super) fn presentation(&self) -> NotificationAccessPresentation {
        use NotificationAccessAction as Action;
        use NotificationAccessState as State;
        use NotificationAccessStatus as Status;

        let (state, state_selector, explanation, action) = match self.status {
            Status::Unsupported => (
                "Unavailable",
                "settings-notifications-access-state-unavailable",
                "Unavailable on this platform.",
                None,
            ),
            Status::Checking => (
                "Checking",
                "settings-notifications-access-state-checking",
                PURPOSE,
                None,
            ),
            Status::State(State::NotDetermined) => (
                "Not requested",
                "settings-notifications-access-state-not-requested",
                PURPOSE,
                Some(Action::Request),
            ),
            Status::Requesting => (
                "Requesting",
                "settings-notifications-access-state-requesting",
                "Respond to the system prompt.",
                Some(Action::Requesting),
            ),
            Status::State(State::Allowed) => (
                "Allowed",
                "settings-notifications-access-state-allowed",
                PURPOSE,
                None,
            ),
            Status::State(State::Quiet) => (
                "Delivered quietly",
                "settings-notifications-access-state-quiet",
                "Notifications appear only in Notification Center, without alerts.",
                Some(Action::OpenSettings),
            ),
            Status::State(State::Hidden) => (
                "Hidden",
                "settings-notifications-access-state-hidden",
                "Alerts and Notification Center are both off, so notifications don\u{2019}t appear.",
                Some(Action::OpenSettings),
            ),
            Status::State(State::Denied) => (
                "Denied",
                "settings-notifications-access-state-denied",
                PURPOSE,
                Some(Action::OpenSettings),
            ),
            Status::State(State::ServiceAvailable) => (
                "Available",
                "settings-notifications-access-state-available",
                "This desktop\u{2019}s notification service shows SpaceTerm\u{2019}s notifications.",
                None,
            ),
            Status::State(State::ServiceUnavailable) => (
                "Unavailable",
                "settings-notifications-access-state-unavailable",
                "No notification service is running on this desktop.",
                Some(Action::CheckAgain),
            ),
            Status::Failed(error) => (
                "Unavailable",
                "settings-notifications-access-state-unavailable",
                match error {
                    NotificationAccessError::Unavailable => "Couldn\u{2019}t check notifications.",
                    NotificationAccessError::Rejected => "Notification request failed.",
                },
                Some(Action::CheckAgain),
            ),
        };
        let explanation = if self.recovery_failure.is_some() && self.opens_settings() {
            open_failed()
        } else {
            explanation
        };
        NotificationAccessPresentation {
            state,
            state_selector,
            explanation,
            action,
        }
    }
}

impl SettingsWindow {
    /// Reads the authorization again. A pending request keeps its status until the system
    /// answers, so a window activation the prompt causes cannot race the decision. Each read
    /// replaces the previous bridge, so only the latest answer lands.
    pub(super) fn refresh_notification_access(&mut self, cx: &mut Context<Self>) {
        let row = &mut self.notification_access;
        if row.status == NotificationAccessStatus::Requesting {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        access.read(Box::new(move |result| {
            let _ = sender.try_send(result);
        }));
        row._read = Some(cx.spawn(async move |settings, cx| {
            // A capability that drops its completion unanswered could not read.
            let result = receiver
                .recv()
                .await
                .unwrap_or(Err(NotificationAccessError::Unavailable));
            let _ = settings.update(cx, |settings, cx| {
                let row = &mut settings.notification_access;
                if row.status != NotificationAccessStatus::Requesting {
                    row.apply(match result {
                        Ok(state) => NotificationAccessStatus::State(state),
                        Err(error) => NotificationAccessStatus::Failed(error),
                    });
                    cx.notify();
                }
            });
        }));
    }

    /// Asks the system for alerts, once, from the Not Determined state, then reads the decision.
    pub(super) fn request_notification_access(&mut self, cx: &mut Context<Self>) {
        let row = &mut self.notification_access;
        if row.status != NotificationAccessStatus::State(NotificationAccessState::NotDetermined) {
            return;
        }
        let Some(access) = row.access.clone() else {
            return;
        };
        let (sender, receiver) = async_channel::bounded(1);
        row.apply(NotificationAccessStatus::Requesting);
        row._read = None;
        access.request(Box::new(move |result| {
            let _ = sender.try_send(result);
        }));
        row._request = Some(cx.spawn(async move |settings, cx| {
            let result = receiver
                .recv()
                .await
                .unwrap_or(Err(NotificationAccessError::Unavailable));
            let _ = settings.update(cx, |settings, cx| {
                match result {
                    Ok(()) => {
                        settings.notification_access.status = NotificationAccessStatus::Checking;
                        settings.refresh_notification_access(cx);
                    }
                    Err(error) => settings
                        .notification_access
                        .apply(NotificationAccessStatus::Failed(error)),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn open_notification_settings(&mut self, cx: &mut Context<Self>) {
        let row = &mut self.notification_access;
        if !row.opens_settings() {
            return;
        }
        let Some(access) = &row.access else {
            return;
        };
        row.recovery_failure = access.open_settings().err();
        cx.notify();
    }

    /// The state badge and, when one applies, the action beside it.
    pub(super) fn render_notification_access(
        &mut self,
        appearance: &ChromeAppearance,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let presentation = self.notification_access.presentation();
        let state_selector = presentation.state_selector;
        let owner = cx.weak_entity();
        let action = presentation.action.map(|action| match action {
            NotificationAccessAction::Request | NotificationAccessAction::Requesting => {
                action_button(
                    REQUEST,
                    "Request Access\u{2026}",
                    action == NotificationAccessAction::Request,
                    move |_, cx| {
                        let _ = owner.update(cx, |settings, cx| {
                            settings.request_notification_access(cx);
                        });
                    },
                )
            }
            NotificationAccessAction::OpenSettings => {
                action_button(OPEN_SETTINGS, "Open System Settings", true, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.open_notification_settings(cx);
                    });
                })
            }
            NotificationAccessAction::CheckAgain => {
                action_button(CHECK_AGAIN, "Check Again", true, move |_, cx| {
                    let _ = owner.update(cx, |settings, cx| {
                        settings.refresh_notification_access(cx);
                    });
                })
            }
        });
        div()
            .debug_selector(|| CONTROL.to_owned())
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

    pub(super) fn render_terminal_attention_notifications(
        &mut self,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.editor.document().notifications.terminal_attention;
        let owner = cx.weak_entity();
        Switch::new(
            TERMINAL_ATTENTION,
            SettingsRowId::TerminalAttentionNotifications
                .descriptor()
                .label(self.permission_access.naming()),
            value,
        )
        .size(ToggleSize::Regular)
        .label_hidden(true)
        .disabled(!self.editor.editable())
        .debug_selector(TERMINAL_ATTENTION)
        .on_change(move |change, _, cx| {
            let value = change.requested();
            let _ = owner.update(cx, |settings, cx| {
                settings.edit(
                    move |draft| draft.notifications.terminal_attention = value,
                    cx,
                );
            });
        })
        .into_any_element()
    }

    pub(super) fn notification_preference_differs(&self) -> bool {
        self.editor.document().notifications != NotificationPreferences::default()
    }

    pub(super) fn reset_notification_preference(&mut self, cx: &mut Context<Self>) {
        self.edit(
            |draft| draft.notifications = NotificationPreferences::default(),
            cx,
        );
    }
}

#[cfg(test)]
#[path = "notifications_tests.rs"]
mod tests;
