//! Freedesktop notifications, with one replaceable attention notification per application.
use super::linux_desktop_events::DesktopEventSender;
use super::linux_session_bus::{
    BusSubscription, RETAINED_REPLY_TIMEOUT, SessionBus, SessionBusError,
};
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
#[derive(Clone, Copy)]
enum Request {
    Show(u32),
    Clear,
}

#[derive(Default)]
struct NotificationState {
    id: Option<u32>,
    token: Option<String>,
    /// The latest request not yet applied.
    request: Option<Request>,
    /// A delivery job is queued or running and applies `request` before it finishes.
    delivering: bool,
}

pub(super) struct LinuxNotificationAdapter {
    bus: Option<SessionBus>,
    /// Notify, CloseNotification, and their signals share one ordered connection that waits for
    /// a slow server's id, so a late id still gets replaced or closed by the next request.
    delivery: Option<SessionBus>,
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
        let delivery = bus
            .as_ref()
            .and_then(|bus| bus.dedicated(RETAINED_REPLY_TIMEOUT).ok());
        let subscription = delivery.as_ref().and_then(|bus| {
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
                            state.id = None;
                            state.token = None;
                        }
                    }
                    _ => {}
                }
            })
            .ok()
        });
        Self {
            bus,
            delivery,
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
        let bus = self
            .delivery
            .as_ref()
            .ok_or(AttentionFailure::Unavailable)?;
        self.request(bus, Request::Show(count))
    }
    fn clear(&self) -> Result<(), AttentionFailure> {
        let Some(bus) = &self.delivery else {
            return Ok(());
        };
        self.request(bus, Request::Clear)
    }
}

impl LinuxNotificationAdapter {
    /// Keeps only the latest request. One delivery job applies it after any in-flight call, so a
    /// late id is replaced or closed as soon as it arrives and a slow server cannot fill the queue.
    fn request(&self, bus: &SessionBus, request: Request) -> Result<(), AttentionFailure> {
        {
            let mut state = lock(&self.state);
            state.request = Some(request);
            if state.delivering {
                return Ok(());
            }
            state.delivering = true;
        }
        let state = self.state.clone();
        let identity = self.identity;
        bus.dispatch(move |connection| deliver(connection, &state, identity))
            .map_err(|error| {
                let mut state = lock(&self.state);
                state.delivering = false;
                state.request = None;
                failure(error)
            })
    }
}

fn lock(state: &Mutex<NotificationState>) -> std::sync::MutexGuard<'_, NotificationState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn deliver(
    connection: &Connection,
    state: &Mutex<NotificationState>,
    identity: ApplicationIdentity,
) {
    loop {
        let (request, replaces) = {
            let mut state = lock(state);
            let Some(request) = state.request.take() else {
                state.delivering = false;
                return;
            };
            // A replacement starts without the previous activation token. A token for the same
            // id that arrives while Notify is outstanding is a real activation and is kept.
            state.token = None;
            if matches!(request, Request::Clear) {
                (request, state.id.take())
            } else {
                (request, state.id)
            }
        };
        let result = match request {
            Request::Show(count) => {
                notify(connection, identity, count, replaces.unwrap_or(0)).map(|id| {
                    let mut state = lock(state);
                    if state.id != Some(id) {
                        state.token = None;
                    }
                    state.id = Some(id);
                })
            }
            Request::Clear => match replaces {
                Some(id) => proxy(connection).and_then(|proxy| {
                    proxy
                        .call::<_, _, ()>("CloseNotification", &(id,))
                        .map_err(Into::into)
                }),
                None => Ok(()),
            },
        };
        if let Err(error) = result {
            eprintln!("desktop notification failed: {error}");
        }
    }
}

fn notify(
    connection: &Connection,
    identity: ApplicationIdentity,
    count: u32,
    replaces: u32,
) -> Result<u32, SessionBusError> {
    let hints = HashMap::from([
        ("desktop-entry", Value::from(identity.application_id())),
        ("urgency", Value::from(1u8)),
    ]);
    proxy(connection)?
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
        .map_err(SessionBusError::from)
}
