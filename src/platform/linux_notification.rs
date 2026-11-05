//! Freedesktop notifications, with one replaceable attention notification per application.
use super::linux_desktop_events::DesktopEventSender;
use super::linux_session_bus::{
    BusSubscription, RETAINED_REPLY_TIMEOUT, SessionBus, SessionBusError,
};
use crate::application_identity::ApplicationIdentity;
use crate::platform::notification_access::{
    NotificationAccess, NotificationAccessError, NotificationAccessState,
    NotificationRequestCompletion, NotificationStateCompletion,
};
use crate::terminal::attention_notification::{
    AuthorizationCompletion, NotificationAdapter, NotificationAuthorization, NotificationSettings,
    SettingsCompletion, notification_body,
};
use crate::terminal::attention_runtime::AttentionFailure;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zbus::blocking::{Connection, Proxy};
use zbus::names::OwnedUniqueName;
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
    owner: Option<OwnedUniqueName>,
    generation: u64,
    id: Option<u32>,
    token: Option<String>,
    /// The latest request not yet applied.
    request: Option<Request>,
    /// A delivery job is queued or running and applies `request` before it finishes.
    delivering: bool,
}

impl NotificationState {
    fn owner_changed(&mut self, old: &str, owner: Option<OwnedUniqueName>) {
        // Discovery can overtake the signal worker. A superseded owner's departure cannot
        // revoke an ID or token issued by the owner that discovery has already established.
        if self
            .owner
            .as_ref()
            .is_none_or(|current| current.as_str() == old)
        {
            self.set_owner(owner);
        }
    }

    fn set_owner(&mut self, owner: Option<OwnedUniqueName>) {
        if self.owner != owner {
            self.owner = owner;
            self.generation = self.generation.wrapping_add(1);
            self.id = None;
            self.token = None;
        }
    }
}

pub(super) struct LinuxNotificationAdapter {
    bus: Option<SessionBus>,
    /// Notify, CloseNotification, and their signals share one ordered connection that waits for
    /// a slow server's id, so a late id still gets replaced or closed by the next request.
    delivery: Option<SessionBus>,
    state: Arc<Mutex<NotificationState>>,
    identity: ApplicationIdentity,
    _subscriptions: Vec<BusSubscription>,
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
        let subscriptions = delivery
            .as_ref()
            .and_then(|bus| {
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
                let owner_state = state.clone();
                let owner_rule = zbus::MatchRule::builder()
                    .msg_type(zbus::message::Type::Signal)
                    .sender("org.freedesktop.DBus")
                    .ok()?
                    .path("/org/freedesktop/DBus")
                    .ok()?
                    .interface("org.freedesktop.DBus")
                    .ok()?
                    .member("NameOwnerChanged")
                    .ok()?
                    .arg(0, NAME)
                    .ok()?
                    .build()
                    .to_owned();
                let owner_subscription = bus
                    .subscribe(owner_rule.into(), move |message| {
                        if let Ok((_, old, new)) =
                            message.body().deserialize::<(String, String, String)>()
                        {
                            let owner = if new.is_empty() {
                                None
                            } else {
                                new.try_into().ok()
                            };
                            lock(&owner_state).owner_changed(&old, owner);
                        }
                    })
                    .ok()?;
                let state = state.clone();
                let signals = bus
                    .subscribe(rule.into(), move |message| {
                        let mut state = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if message.header().sender().map(|sender| sender.as_str())
                            != state.owner.as_ref().map(|owner| owner.as_str())
                        {
                            return;
                        }
                        match message.header().member().map(|name| name.as_str()) {
                            Some("ActivationToken") => {
                                if let Ok((id, token)) =
                                    message.body().deserialize::<(u32, String)>()
                                    && state.id == Some(id)
                                    && token.len() <= 4096
                                {
                                    state.token = Some(token);
                                }
                            }
                            Some("ActionInvoked") => {
                                if let Ok((id, action)) =
                                    message.body().deserialize::<(u32, String)>()
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
                    .ok()?;
                Some(vec![owner_subscription, signals])
            })
            .unwrap_or_default();
        // Delivery needs both observers to retain service-owned authority safely.
        let delivery = if subscriptions.len() == 2 {
            delivery
        } else {
            None
        };

        Self {
            bus,
            delivery,
            identity,
            state,
            _subscriptions: subscriptions,
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
/// Asks the session bus whether a notification service answers. `completion` runs once.
fn server_information(
    bus: Option<&SessionBus>,
    completion: Box<dyn FnOnce(Result<(), AttentionFailure>) + Send>,
) {
    let Some(bus) = bus else {
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
                Ok(())
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

impl NotificationAdapter for LinuxNotificationAdapter {
    fn settings(&self, completion: SettingsCompletion) {
        server_information(
            self.bus.as_ref(),
            Box::new(move |result| {
                completion(result.map(|()| NotificationSettings {
                    authorization: NotificationAuthorization::Authorized,
                    alert_enabled: true,
                    center_enabled: true,
                }));
            }),
        );
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

/// The Privacy section's view of notifications. Linux grants no per-application authorization,
/// so it reports only whether a notification service answers.
pub(super) struct LinuxNotificationAccess {
    bus: Option<SessionBus>,
}

impl LinuxNotificationAccess {
    pub(super) const fn new(bus: Option<SessionBus>) -> Self {
        Self { bus }
    }
}

impl NotificationAccess for LinuxNotificationAccess {
    fn read(&self, completion: NotificationStateCompletion) {
        server_information(
            self.bus.as_ref(),
            Box::new(move |result| {
                completion(Ok(match result {
                    Ok(()) => NotificationAccessState::ServiceAvailable,
                    Err(_) => NotificationAccessState::ServiceUnavailable,
                }));
            }),
        );
    }

    fn request(&self, completion: NotificationRequestCompletion) {
        completion(Err(NotificationAccessError::Unavailable));
    }

    fn open_settings(&self) -> Result<(), NotificationAccessError> {
        Err(NotificationAccessError::Unavailable)
    }
}

fn lock(state: &Mutex<NotificationState>) -> std::sync::MutexGuard<'_, NotificationState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Reads ownership before and after a retained-object call. Signals invalidate immediately;
/// the bounded query also closes the race when the signal worker has not caught up yet.
fn service_owner(
    connection: &Connection,
    state: &Mutex<NotificationState>,
    activate: bool,
) -> Result<(OwnedUniqueName, u64), SessionBusError> {
    for _ in 0..3 {
        let generation = lock(state).generation;
        let dbus =
            zbus::blocking::fdo::DBusProxy::new(connection).map_err(SessionBusError::from)?;
        let read = || {
            dbus.get_name_owner(NAME.try_into().expect("static bus name"))
                .map_err(|error| SessionBusError::from(zbus::Error::FDO(Box::new(error))))
        };
        let owner = read().or_else(|error| {
            if activate && error == SessionBusError::Unavailable {
                let _: (String, String, String, String) = proxy(connection)?
                    .call("GetServerInformation", &())
                    .map_err(SessionBusError::from)?;
                read()
            } else {
                Err(error)
            }
        });
        let mut state = lock(state);
        if state.generation != generation {
            continue;
        }
        match owner {
            Ok(owner) => {
                state.set_owner(Some(owner.clone()));
                return Ok((owner, state.generation));
            }
            Err(error) => {
                if error == SessionBusError::Unavailable {
                    state.set_owner(None);
                }
                return Err(error);
            }
        }
    }
    Err(SessionBusError::Rejected)
}

fn deliver(
    connection: &Connection,
    state: &Mutex<NotificationState>,
    identity: ApplicationIdentity,
) {
    loop {
        let request = {
            let mut state = lock(state);
            let Some(request) = state.request.take() else {
                state.delivering = false;
                return;
            };
            request
        };
        let result = (|| {
            let (owner, generation) =
                service_owner(connection, state, matches!(request, Request::Show(_)))?;
            let replaces = {
                let mut state = lock(state);
                if state.generation != generation {
                    return Err(SessionBusError::Unavailable);
                }
                // Only a token from this service arriving during replacement may survive.
                state.token = None;
                if matches!(request, Request::Clear) {
                    state.id.take()
                } else {
                    state.id
                }
            };
            match request {
                Request::Show(count) => {
                    let id = notify(connection, &owner, identity, count, replaces.unwrap_or(0))?;
                    let current = service_owner(connection, state, false)?;
                    let mut state = lock(state);
                    if current == (owner, generation) && state.generation == generation {
                        if state.id != Some(id) {
                            state.token = None;
                        }
                        state.id = Some(id);
                    }
                }
                Request::Clear => {
                    if let Some(id) = replaces {
                        Proxy::new(connection, owner.as_str(), PATH, NAME)
                            .map_err(SessionBusError::from)?
                            .call::<_, _, ()>("CloseNotification", &(id,))
                            .map_err(SessionBusError::from)?;
                    }
                }
            }
            Ok(())
        })();
        let mut state = lock(state);
        let failure = result.err().map(failure);
        if failure.is_some() {
            // A rejected replaces_id supplies no authority for the next request.
            state.id = None;
            state.token = None;
        }
    }
}

fn notify(
    connection: &Connection,
    owner: &OwnedUniqueName,
    identity: ApplicationIdentity,
    count: u32,
    replaces: u32,
) -> Result<u32, SessionBusError> {
    let hints = HashMap::from([
        ("desktop-entry", Value::from(identity.application_id())),
        ("urgency", Value::from(1u8)),
    ]);
    Proxy::new(connection, owner.as_str(), PATH, NAME)
        .map_err(SessionBusError::from)?
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

/// Freedesktop notifications for overdue updates, one per reminder.
pub(super) struct LinuxUpdateReminders {
    bus: Option<SessionBus>,
    identity: ApplicationIdentity,
    /// The reminder key of each notification this service showed, by its id.
    shown: Arc<Mutex<HashMap<u32, String>>>,
    _subscription: std::cell::RefCell<Option<BusSubscription>>,
}

impl LinuxUpdateReminders {
    pub(super) fn new(bus: Option<SessionBus>, identity: ApplicationIdentity) -> Self {
        Self {
            bus,
            identity,
            shown: Arc::default(),
            _subscription: std::cell::RefCell::new(None),
        }
    }
}

impl crate::updates::reminders::UpdateReminderAdapter for LinuxUpdateReminders {
    /// Follows ActionInvoked for the reminders' own ids. Another service's signal, or one for an
    /// id this service did not show, is ignored.
    fn activations(&self, activated: async_channel::Sender<String>) {
        let Some(bus) = &self.bus else { return };
        let Some(rule) = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .path(PATH)
            .ok()
            .and_then(|rule| rule.interface(NAME).ok())
            .and_then(|rule| rule.member("ActionInvoked").ok())
            .map(|rule| rule.build().to_owned())
        else {
            return;
        };
        let shown = Arc::clone(&self.shown);
        let subscription = bus.subscribe(rule.into(), move |message| {
            if let Ok((id, action)) = message.body().deserialize::<(u32, String)>()
                && action == "default"
                && let Some(key) = lock_shown(&shown).remove(&id)
            {
                let _ = activated.try_send(key);
            }
        });
        *self._subscription.borrow_mut() = subscription.ok();
    }

    fn notify(&self, key: &str, title: &str, body: &str) {
        let Some(bus) = &self.bus else { return };
        let identity = self.identity;
        let shown = Arc::clone(&self.shown);
        let (key, title, body) = (key.to_owned(), title.to_owned(), body.to_owned());
        let _ = bus.dispatch(move |connection| {
            let hints = HashMap::from([
                ("desktop-entry", Value::from(identity.application_id())),
                ("urgency", Value::from(1u8)),
            ]);
            // Replaces this key's earlier reminder, if the service still shows it.
            let replaces = lock_shown(&shown)
                .iter()
                .find_map(|(id, shown)| (*shown == key).then_some(*id))
                .unwrap_or(0);
            let id = Proxy::new(connection, NAME, PATH, NAME).and_then(|proxy| {
                proxy.call::<_, _, u32>(
                    "Notify",
                    &(
                        identity.display_name(),
                        replaces,
                        "",
                        title.as_str(),
                        body.as_str(),
                        vec!["default", "Open"],
                        hints,
                        -1i32,
                    ),
                )
            });
            if let Ok(id) = id {
                let mut shown = lock_shown(&shown);
                shown.retain(|_, shown| *shown != key);
                shown.insert(id, key);
            }
        });
    }
}

fn lock_shown(
    shown: &Mutex<HashMap<u32, String>>,
) -> std::sync::MutexGuard<'_, HashMap<u32, String>> {
    shown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_notification_delayed_departure_cannot_revoke_a_new_service_id() {
        let mut state = NotificationState::default();
        state.set_owner(Some(":1.7".try_into().unwrap()));
        // The delivery query has already observed the replacement before its signal is handled.
        let replacement: OwnedUniqueName = ":1.8".try_into().unwrap();
        state.set_owner(Some(replacement.clone()));
        state.id = Some(43);
        state.token = Some("replacement-token".into());
        let generation = state.generation;
        state.owner_changed(":1.7", None);
        state.owner_changed("", Some(replacement));
        assert_eq!(
            (state.id, state.token.as_deref()),
            (Some(43), Some("replacement-token"))
        );
        assert_eq!(state.generation, generation);
        state.owner_changed(":1.8", None);
        assert_eq!((state.id, state.token.as_deref()), (None, None));
    }

    #[test]
    fn linux_notification_service_departure_retires_id_and_activation_token() {
        let mut state = NotificationState::default();
        let owner: OwnedUniqueName = ":1.7".try_into().unwrap();
        state.set_owner(Some(owner.clone()));
        state.id = Some(42);
        state.token = Some("fixture-token".into());
        let generation = state.generation;
        state.set_owner(None);
        assert_eq!((state.id, state.token.as_deref()), (None, None));
        assert_ne!(state.generation, generation);
        let departed_generation = state.generation;
        state.set_owner(Some(owner));
        assert_ne!(state.generation, generation);
        assert_ne!(
            state.generation, departed_generation,
            "reacquisition has a new owner generation"
        );
        state.id = Some(42);
        state.token = Some("fixture-token".into());
        state.set_owner(Some(":1.8".try_into().unwrap()));
        assert_eq!((state.id, state.token.as_deref()), (None, None));
    }
}
