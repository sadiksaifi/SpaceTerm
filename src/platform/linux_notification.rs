//! Freedesktop notifications, with one replaceable attention notification per application.
use super::linux_desktop_events::DesktopEventSender;
use super::linux_session_bus::{BusSubscription, SessionBus, SessionBusError};
use crate::application_identity::ApplicationIdentity;
use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion, notification_body,
};
use crate::terminal::attention_runtime::AttentionFailure;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

const NAME: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
#[derive(Default)]
struct NotificationState {
    id: Option<u32>,
    token: Option<String>,
}

pub(super) struct LinuxNotificationAdapter {
    bus: Option<SessionBus>,
    state: Arc<Mutex<NotificationState>>,
    identity: ApplicationIdentity,
    _subscription: Option<BusSubscription>,
}
impl LinuxNotificationAdapter {
    pub(super) fn new(
        bus: Option<SessionBus>,
        identity: ApplicationIdentity,
        events: DesktopEventSender,
    ) -> Self {
        let state = Arc::new(Mutex::new(NotificationState::default()));
        let subscription = bus.as_ref().and_then(|bus| {
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(NAME)
                .ok()?
                .path(PATH)
                .ok()?
                .interface(NAME)
                .ok()?
                .build()
                .to_owned();
            let state = state.clone();
            bus.subscribe(rule.into(), move |message| {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match message.header().member().map(|name| name.as_str()) {
                    Some("ActivationToken") => {
                        if let Ok((id, token)) = message.body().deserialize::<(u32, String)>()
                            && state.id == Some(id)
                            && token.len() <= 4096
                        {
                            state.token = Some(token);
                        }
                    }
                    Some("ActionInvoked") => {
                        if let Ok((id, action)) = message.body().deserialize::<(u32, String)>()
                            && state.id == Some(id)
                            && action == "default"
                        {
                            events.activate(state.token.take());
                        }
                    }
                    Some("NotificationClosed") => {
                        if let Ok((id, _)) = message.body().deserialize::<(u32, u32)>()
                            && state.id == Some(id)
                        {
                            *state = NotificationState::default();
                        }
                    }
                    _ => {}
                }
            })
            .ok()
        });
        Self {
            bus,
            identity,
            state,
            _subscription: subscription,
        }
    }
}
fn proxy(connection: &Connection) -> Result<Proxy<'_>, SessionBusError> {
    Proxy::new(connection, NAME, PATH, NAME).map_err(Into::into)
}
fn failure(error: SessionBusError) -> AttentionFailure {
    match error {
        SessionBusError::Unavailable => AttentionFailure::Unavailable,
        _ => AttentionFailure::DeliveryFailed,
    }
}
impl NotificationAdapter for LinuxNotificationAdapter {
    fn settings(&self, completion: SettingsCompletion) {
        let Some(bus) = &self.bus else {
            completion(Err(AttentionFailure::Unavailable));
            return;
        };
        let completion = Arc::new(Mutex::new(Some(completion)));
        let callback = completion.clone();
        let result = bus.dispatch(move |connection| {
            let result = proxy(connection)
                .and_then(|proxy| {
                    let _: (String, String, String, String) = proxy
                        .call("GetServerInformation", &())
                        .map_err(SessionBusError::from)?;
                    Ok(NotificationSettings {
                        authorization: NotificationAuthorization::Authorized,
                        alert_enabled: true,
                        center_enabled: true,
                    })
                })
                .map_err(failure);
            if let Some(completion) = callback
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
            {
                completion(result);
            }
        });
        if let Err(error) = result
            && let Some(completion) = completion
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        {
            completion(Err(failure(error)));
        }
    }
    fn authorize_provisionally(&self, completion: AuthorizationCompletion) {
        completion(Ok(()));
    }
    fn submit(&self, count: u32) -> Result<(), AttentionFailure> {
        let bus = self.bus.as_ref().ok_or(AttentionFailure::Unavailable)?;
        let state = self.state.clone();
        let identity = self.identity;
        bus.dispatch(move |connection| {
            let result = (|| {
                let replaces = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .id
                    .unwrap_or(0);
                let hints = HashMap::from([
                    ("desktop-entry", Value::from(identity.application_id())),
                    ("urgency", Value::from(1u8)),
                ]);
                let id: u32 = proxy(connection)?
                    .call(
                        "Notify",
                        &(
                            identity.display_name(),
                            replaces,
                            "",
                            identity.display_name(),
                            notification_body(count),
                            vec!["default", "Open"],
                            hints,
                            -1i32,
                        ),
                    )
                    .map_err(SessionBusError::from)?;
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.id = Some(id);
                state.token = None;
                Ok::<_, SessionBusError>(())
            })();
            if let Err(error) = result {
                eprintln!("desktop notification failed: {error}");
            }
        })
        .map_err(failure)
    }
    fn clear(&self) -> Result<(), AttentionFailure> {
        let Some(bus) = &self.bus else {
            return Ok(());
        };
        let state = self.state.clone();
        bus.dispatch(move |connection| {
            let id = {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.token = None;
                state.id.take()
            };
            if let Some(id) = id {
                let result = proxy(connection).and_then(|proxy| {
                    proxy
                        .call::<_, _, ()>("CloseNotification", &(id,))
                        .map_err(Into::into)
                });
                if let Err(error) = result {
                    eprintln!("desktop notification close failed: {error}");
                }
            }
        })
        .map_err(failure)
    }
}
