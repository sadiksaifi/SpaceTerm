//! The Linux status area: a StatusNotifierItem with a DBusMenu, registered with the
//! session's StatusNotifierWatcher. A session without a watcher and host has no status area,
//! and a session whose watcher or host leaves and does not return within a grace period
//! loses it. The grace period does not run while logind reports the session locked or idle,
//! because GNOME Shell removes its watcher for as long as the user is away.

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, watch};
use zbus::export::futures_core::Stream;
use zbus::fdo::{DBusProxy, NameOwnerChangedStream, RequestNameFlags, RequestNameReply};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str, Structure};
use zbus::{MatchRule, MessageStream};

use super::CompanionIdentity;
use super::controller::{
    ActionIngress, CompanionAction, CompanionController, CompanionDependencies, CompanionFailure,
    DesktopActivation, StatusArea, StatusSource,
};
use super::model::{CompanionModel, MenuEntry, mnemonic_label};
use super::native::{
    ActivationToken, ModelSlot, TokioTimer, UnavailableNativeStatus, sibling_desktop_executable,
};

const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const LOGIN: &str = "org.freedesktop.login1";
const LOGIN_PATH: &str = "/org/freedesktop/login1";
const LOGIN_MANAGER: &str = "org.freedesktop.login1.Manager";
const LOGIN_SESSION: &str = "org.freedesktop.login1.Session";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const BUS_DEADLINE: Duration = Duration::from_secs(5);
/// How long a panel restart may leave the item without a watcher or host before the
/// companion reports the status area lost instead of running invisibly.
const HOST_GRACE: Duration = Duration::from_secs(30);
/// Watcher and login session signals only prompt a fresh query, so a few queued ones suffice.
const WATCHER_SIGNAL_QUEUE: usize = 8;
/// Repeated Open commands within this window start one desktop process.
const LAUNCH_COALESCING: Duration = Duration::from_secs(2);
const ROOT: i32 = 0;

type Properties = HashMap<String, OwnedValue>;
type Layout = (i32, Properties, Vec<OwnedValue>);
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;
type ToolTip = (String, Pixmaps, String, String);
type MenuEvent = (i32, String, OwnedValue, u32);

/// What both D-Bus objects publish. Only the update task replaces the model.
struct MenuState {
    identity: CompanionIdentity,
    model: CompanionModel,
    revision: u32,
}

impl MenuState {
    fn entries(&self) -> Vec<MenuEntry> {
        self.model.menu(self.identity)
    }

    fn entry(&self, id: i32) -> Option<MenuEntry> {
        self.entries().into_iter().find(|entry| entry.id() == id)
    }

    fn layout(&self, parent: i32) -> Option<Layout> {
        if parent == ROOT {
            let children = self
                .entries()
                .iter()
                .filter_map(|entry| {
                    let child: Layout = (entry.id(), properties(entry), Vec::new());
                    OwnedValue::try_from(Structure::from(child)).ok()
                })
                .collect();
            let mut root = Properties::new();
            root.insert("children-display".to_owned(), text("submenu"));
            return Some((ROOT, root, children));
        }
        let entry = self.entry(parent)?;
        Some((entry.id(), properties(&entry), Vec::new()))
    }
}

fn text(value: &str) -> OwnedValue {
    OwnedValue::from(Str::from(value.to_owned()))
}

/// DBusMenu treats one underscore as an access key marker, so literal text doubles it.
fn literal(value: &str) -> OwnedValue {
    text(&value.replace('_', "__"))
}

fn properties(entry: &MenuEntry) -> Properties {
    let mut properties = Properties::new();
    match entry {
        MenuEntry::Header { text: line, .. } | MenuEntry::Information { text: line, .. } => {
            properties.insert("label".to_owned(), literal(line));
            properties.insert("enabled".to_owned(), OwnedValue::from(false));
            properties.insert("accessible-desc".to_owned(), text(line));
        }
        MenuEntry::Separator { .. } => {
            properties.insert("type".to_owned(), text("separator"));
        }
        MenuEntry::Action {
            title, mnemonic, ..
        } => {
            properties.insert("label".to_owned(), text(&mnemonic_label(title, *mnemonic)));
            properties.insert("enabled".to_owned(), OwnedValue::from(true));
            properties.insert("accessible-desc".to_owned(), text(title));
        }
    }
    properties
}

/// A bus failure is reported as an unavailable status area, without its native detail.
fn unavailable<E>(_: E) -> CompanionFailure {
    CompanionFailure::StatusAreaUnavailable
}

fn locked(state: &Mutex<MenuState>) -> std::sync::MutexGuard<'_, MenuState> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

struct StatusNotifierItem {
    state: Arc<Mutex<MenuState>>,
    actions: Arc<ActionIngress>,
    token: ActivationToken,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusNotifierItem {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        locked(&self.state).identity.application_id()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        locked(&self.state).identity.display_name()
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        locked(&self.state).model.symbol().freedesktop().to_owned()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Pixmaps {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> &str {
        ""
    }

    /// Hosts use the title and text as the item's accessible name and description. The text
    /// holds every status line, because keyboard focus skips disabled menu items.
    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        let state = locked(&self.state);
        (
            state.model.symbol().freedesktop().to_owned(),
            Vec::new(),
            state.identity.display_name(),
            state.model.description(state.identity),
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).unwrap_or_default()
    }

    fn context_menu(&self, _x: i32, _y: i32) {}

    /// A host that activates instead of opening the menu opens the desktop application.
    fn activate(&self, _x: i32, _y: i32) {
        let _ = self.actions.push(CompanionAction::OpenDesktop);
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: String) {}

    fn provide_xdg_activation_token(&self, token: String) {
        self.token.provide(token);
    }

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct DbusMenu {
    state: Arc<Mutex<MenuState>>,
    actions: Arc<ActionIngress>,
}

impl DbusMenu {
    fn activate(&self, id: i32, event: &str) -> bool {
        let Some(entry) = locked(&self.state).entry(id) else {
            return false;
        };
        if event == "clicked"
            && let MenuEntry::Action { action, .. } = entry
        {
            let _ = self.actions.push(action);
        }
        true
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }

    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(out_args("revision", "layout"))]
    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let state = locked(&self.state);
        let layout = state
            .layout(parent_id)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown menu item".to_owned()))?;
        Ok((state.revision, layout))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Properties)> {
        let entries = locked(&self.state).entries();
        entries
            .iter()
            .filter(|entry| ids.is_empty() || ids.contains(&entry.id()))
            .map(|entry| {
                let mut properties = properties(entry);
                if !property_names.is_empty() {
                    properties.retain(|name, _| property_names.contains(name));
                }
                (entry.id(), properties)
            })
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        locked(&self.state)
            .entry(id)
            .and_then(|entry| properties(&entry).remove(&name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown menu property".to_owned()))
    }

    fn event(
        &self,
        id: i32,
        event_id: String,
        _data: OwnedValue,
        _timestamp: u32,
    ) -> zbus::fdo::Result<()> {
        if self.activate(id, &event_id) {
            Ok(())
        } else {
            Err(zbus::fdo::Error::InvalidArgs(
                "unknown menu item".to_owned(),
            ))
        }
    }

    fn event_group(&self, events: Vec<MenuEvent>) -> Vec<i32> {
        events
            .into_iter()
            .take(ActionIngress::CAPACITY)
            .filter(|(id, event, _, _)| !self.activate(*id, event))
            .map(|(id, _, _, _)| id)
            .collect()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(out_args("updatesNeeded", "idErrors"))]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let state = locked(&self.state);
        let errors = ids
            .into_iter()
            .filter(|id| *id != ROOT && state.entry(*id).is_none())
            .collect();
        (Vec::new(), errors)
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

/// The controller's view of the registered item.
pub(crate) struct LinuxStatusArea {
    slot: Arc<ModelSlot>,
    wake: Arc<Notify>,
    actions: Arc<ActionIngress>,
    lost: Arc<AtomicBool>,
    task: tokio::task::JoinHandle<()>,
    _connection: zbus::Connection,
}

impl Drop for LinuxStatusArea {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl LinuxStatusArea {
    /// Claims the companion's bus name, then registers with a watcher that has a host.
    /// `address` selects a bus other than the session bus, and `login` a bus other than the
    /// system bus for logind. After registration, the area is lost when no watcher with a
    /// host has served it for `grace` while the session is present.
    pub(crate) async fn connect(
        identity: CompanionIdentity,
        actions: Arc<ActionIngress>,
        token: ActivationToken,
        address: Option<String>,
        login: Option<String>,
        grace: Duration,
    ) -> Result<Self, CompanionFailure> {
        tokio::time::timeout(
            BUS_DEADLINE,
            Self::establish(identity, actions, token, address, login, grace),
        )
        .await
        .map_err(|_| CompanionFailure::StatusAreaUnavailable)?
    }

    async fn establish(
        identity: CompanionIdentity,
        actions: Arc<ActionIngress>,
        token: ActivationToken,
        address: Option<String>,
        login: Option<String>,
        grace: Duration,
    ) -> Result<Self, CompanionFailure> {
        let state = Arc::new(Mutex::new(MenuState {
            identity,
            model: CompanionModel::connecting(),
            revision: 1,
        }));
        let builder = match &address {
            Some(address) => zbus::connection::Builder::address(address.as_str()),
            None => zbus::connection::Builder::session(),
        }
        .map_err(unavailable)?;
        let connection = builder
            .serve_at(
                ITEM_PATH,
                StatusNotifierItem {
                    state: state.clone(),
                    actions: actions.clone(),
                    token,
                },
            )
            .map_err(unavailable)?
            .serve_at(
                MENU_PATH,
                DbusMenu {
                    state: state.clone(),
                    actions: actions.clone(),
                },
            )
            .map_err(unavailable)?
            .build()
            .await
            .map_err(unavailable)?;
        let name = identity.application_id();
        match connection
            .request_name_with_flags(name.as_str(), RequestNameFlags::DoNotQueue.into())
            .await
        {
            Ok(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner) => {}
            Ok(_) | Err(zbus::Error::NameTaken) => return Err(CompanionFailure::AlreadyRunning),
            Err(_) => return Err(CompanionFailure::StatusAreaUnavailable),
        }
        let watcher_changes = DBusProxy::new(&connection)
            .await
            .map_err(unavailable)?
            .receive_name_owner_changed_with_args(&[(0, WATCHER)])
            .await
            .map_err(unavailable)?;
        // Any process may emit these signals, so they only prompt a query of the watcher.
        let watcher_signals = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(WATCHER)
            .and_then(|rule| rule.path(WATCHER_PATH))
            .map_err(unavailable)?
            .build();
        let watcher_signals =
            MessageStream::for_match_rule(watcher_signals, &connection, Some(WATCHER_SIGNAL_QUEUE))
                .await
                .map_err(unavailable)?;
        if !hosted(&connection).await? {
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
        register(&connection, &name).await?;
        let slot = Arc::new(ModelSlot::new());
        let wake = Arc::new(Notify::new());
        let lost = Arc::new(AtomicBool::new(false));
        let presence = SessionPresence::follow(login);
        let task = tokio::spawn(follow_bus(Bus {
            connection: connection.clone(),
            presence,
            state,
            slot: slot.clone(),
            wake: wake.clone(),
            watcher_changes: Box::pin(watcher_changes),
            watcher_signals: Box::pin(watcher_signals),
            name,
            grace,
            lost: lost.clone(),
            actions: actions.clone(),
        }));
        Ok(Self {
            slot,
            wake,
            actions,
            lost,
            task,
            _connection: connection,
        })
    }
}

impl StatusArea for LinuxStatusArea {
    fn present(&mut self, model: &CompanionModel) -> Result<(), CompanionFailure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
        if self.slot.replace(model.clone()) {
            self.wake.notify_one();
        }
        Ok(())
    }

    fn next_action(&mut self) -> Result<Option<CompanionAction>, CompanionFailure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
        Ok(self.actions.pop())
    }
}

/// Whether the session's watcher reports a host that displays registered items.
async fn hosted(connection: &zbus::Connection) -> Result<bool, CompanionFailure> {
    let reply = tokio::time::timeout(
        BUS_DEADLINE,
        connection.call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(WATCHER, "IsStatusNotifierHostRegistered"),
        ),
    )
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    let hosted: OwnedValue = reply.body().deserialize().map_err(unavailable)?;
    bool::try_from(hosted).map_err(unavailable)
}

/// Registers the item with the session's current watcher.
async fn register(connection: &zbus::Connection, name: &str) -> Result<(), CompanionFailure> {
    tokio::time::timeout(
        BUS_DEADLINE,
        connection.call_method(
            Some(WATCHER),
            WATCHER_PATH,
            Some(WATCHER),
            "RegisterStatusNotifierItem",
            &(name,),
        ),
    )
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    Ok(())
}

struct Bus {
    connection: zbus::Connection,
    presence: SessionPresence,
    state: Arc<Mutex<MenuState>>,
    slot: Arc<ModelSlot>,
    wake: Arc<Notify>,
    watcher_changes: Pin<Box<NameOwnerChangedStream>>,
    watcher_signals: Pin<Box<MessageStream>>,
    name: String,
    grace: Duration,
    lost: Arc<AtomicBool>,
    actions: Arc<ActionIngress>,
}

enum BusEvent {
    Model,
    WatcherReturned,
    WatcherLeft,
    WatcherSignaled,
    Away(bool),
    Unhosted,
    Closed,
}

/// Publishes the newest model, registers again when a watcher replaces the old one, and
/// follows whether a host displays the item. Losing the bus, or going without a watcher
/// and host for the grace period while the session is present, marks the area lost and
/// wakes the controller.
async fn follow_bus(mut bus: Bus) {
    // Whether no watcher with a host shows the item.
    let mut unhosted = false;
    // Whether logind reports the session locked or idle.
    let mut away = false;
    // Set while unhosted and present: the instant the area is given up.
    let mut unhosted_until: Option<tokio::time::Instant> = None;
    loop {
        let watcher_changes = &mut bus.watcher_changes;
        let watcher_signals = &mut bus.watcher_signals;
        let presence = &mut bus.presence;
        let deadline = async {
            match unhosted_until {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        let event = tokio::select! {
            () = bus.wake.notified() => BusEvent::Model,
            change = std::future::poll_fn(|context| watcher_changes.as_mut().poll_next(context)) => {
                match change {
                    None => BusEvent::Closed,
                    Some(change) => match change.args() {
                        Ok(arguments) if arguments.new_owner().is_some() => BusEvent::WatcherReturned,
                        _ => BusEvent::WatcherLeft,
                    },
                }
            }
            signal = std::future::poll_fn(|context| watcher_signals.as_mut().poll_next(context)) => {
                match signal {
                    None => BusEvent::Closed,
                    Some(_) => BusEvent::WatcherSignaled,
                }
            }
            now_away = presence.changed() => BusEvent::Away(now_away),
            () = deadline => BusEvent::Unhosted,
        };
        let shown = match event {
            BusEvent::Model => {
                if publish(&bus).await.is_err() {
                    break;
                }
                continue;
            }
            BusEvent::Unhosted | BusEvent::Closed => break,
            BusEvent::Away(now_away) => {
                let returned = away && !now_away;
                away = now_away;
                if away {
                    unhosted_until = None;
                }
                // A watcher that returned while the user was away may not have taken the
                // item yet, so a returning session registers again before a fresh grace.
                if !(returned && unhosted) {
                    continue;
                }
                registered_and_hosted(&bus).await
            }
            BusEvent::WatcherLeft => false,
            BusEvent::WatcherReturned => registered_and_hosted(&bus).await,
            // A watcher that could not take the item earlier gets it once its host appears.
            BusEvent::WatcherSignaled if unhosted_until.is_some() => {
                registered_and_hosted(&bus).await
            }
            BusEvent::WatcherSignaled => matches!(hosted(&bus.connection).await, Ok(true)),
        };
        unhosted = !shown;
        if !unhosted || away {
            unhosted_until = None;
        } else {
            unhosted_until.get_or_insert_with(|| tokio::time::Instant::now() + bus.grace);
        }
    }
    bus.lost.store(true, Ordering::Release);
    bus.actions.interrupt();
}

/// Whether logind reports the user's session locked or idle. GNOME Shell disables its
/// StatusNotifierWatcher extension on the lock screen, and on the idle shield when locking
/// is off, so its watcher is gone for as long as the user is away. Without logind, or when
/// it stops answering, the session counts as present and the grace period applies.
struct SessionPresence {
    away: watch::Receiver<bool>,
    open: bool,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for SessionPresence {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl SessionPresence {
    fn follow(address: Option<String>) -> Self {
        let (sender, away) = watch::channel(false);
        let task = tokio::spawn(async move {
            let _ = follow_session(address, &sender).await;
            sender.send_if_modified(|away| std::mem::replace(away, false));
        });
        Self {
            away,
            open: true,
            task,
        }
    }

    /// The next change of whether the session is away. Pending forever once logind is gone.
    async fn changed(&mut self) -> bool {
        if self.open && self.away.changed().await.is_ok() {
            return *self.away.borrow_and_update();
        }
        self.open = false;
        std::future::pending().await
    }
}

/// Follows the session logind resolves for this process. "auto" is the caller's session, or
/// its user's display session when the caller runs outside one, as an application scope
/// started by the desktop does.
async fn follow_session(
    address: Option<String>,
    away: &watch::Sender<bool>,
) -> Result<(), CompanionFailure> {
    let connection = tokio::time::timeout(BUS_DEADLINE, async {
        match &address {
            Some(address) => zbus::connection::Builder::address(address.as_str()),
            None => zbus::connection::Builder::system(),
        }?
        .build()
        .await
    })
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    let reply = tokio::time::timeout(
        BUS_DEADLINE,
        connection.call_method(
            Some(LOGIN),
            LOGIN_PATH,
            Some(LOGIN_MANAGER),
            "GetSession",
            &("auto",),
        ),
    )
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    let session: OwnedObjectPath = reply.body().deserialize().map_err(unavailable)?;
    // Signals only prompt a query of logind, so another sender cannot claim a lock.
    let changes = MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(LOGIN)
        .and_then(|rule| rule.interface("org.freedesktop.DBus.Properties"))
        .and_then(|rule| rule.member("PropertiesChanged"))
        .and_then(|rule| rule.path(session.clone()))
        .map_err(unavailable)?
        .build();
    let mut changes =
        MessageStream::for_match_rule(changes, &connection, Some(WATCHER_SIGNAL_QUEUE))
            .await
            .map_err(unavailable)?;
    loop {
        let locked = session_hint(&connection, &session, "LockedHint").await?;
        let idle = session_hint(&connection, &session, "IdleHint").await?;
        let now_away = locked || idle;
        away.send_if_modified(|away| std::mem::replace(away, now_away) != now_away);
        let change =
            std::future::poll_fn(|context| Pin::new(&mut changes).poll_next(context)).await;
        if change.is_none() {
            return Err(CompanionFailure::StatusAreaUnavailable);
        }
    }
}

async fn session_hint(
    connection: &zbus::Connection,
    session: &OwnedObjectPath,
    hint: &str,
) -> Result<bool, CompanionFailure> {
    let reply = tokio::time::timeout(
        BUS_DEADLINE,
        connection.call_method(
            Some(LOGIN),
            session.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &(LOGIN_SESSION, hint),
        ),
    )
    .await
    .map_err(unavailable)?
    .map_err(unavailable)?;
    let value: OwnedValue = reply.body().deserialize().map_err(unavailable)?;
    bool::try_from(value).map_err(unavailable)
}

/// Registers the item with the current watcher and reports whether a host shows it.
async fn registered_and_hosted(bus: &Bus) -> bool {
    register(&bus.connection, &bus.name).await.is_ok()
        && matches!(hosted(&bus.connection).await, Ok(true))
}

async fn publish(bus: &Bus) -> zbus::Result<()> {
    let Some(model) = bus.slot.take() else {
        return Ok(());
    };
    let revision = {
        let mut state = locked(&bus.state);
        state.model = model;
        state.revision = state.revision.wrapping_add(1);
        state.revision
    };
    let server = bus.connection.object_server();
    let item = server.interface::<_, StatusNotifierItem>(ITEM_PATH).await?;
    StatusNotifierItem::new_icon(item.signal_emitter()).await?;
    StatusNotifierItem::new_tool_tip(item.signal_emitter()).await?;
    let menu = server.interface::<_, DbusMenu>(MENU_PATH).await?;
    DbusMenu::layout_updated(menu.signal_emitter(), revision, ROOT).await
}

/// Starts the desktop executable installed beside this companion, handing over the
/// host's activation token so the compositor can focus the new window.
struct SiblingDesktop {
    executable: Option<PathBuf>,
    token: ActivationToken,
    last_launch: Mutex<Option<Instant>>,
}

impl DesktopActivation for SiblingDesktop {
    fn open(&self) -> Result<(), CompanionFailure> {
        let executable = self
            .executable
            .as_ref()
            .filter(|executable| executable.is_file())
            .ok_or(CompanionFailure::DesktopUnavailable)?;
        let mut last_launch = self
            .last_launch
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if last_launch.is_some_and(|launched| launched.elapsed() < LAUNCH_COALESCING) {
            return Ok(());
        }
        let mut command = Command::new(executable);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(token) = self.token.take() {
            command.env("XDG_ACTIVATION_TOKEN", token);
        }
        let mut child = command
            .spawn()
            .map_err(|_| CompanionFailure::DesktopUnavailable)?;
        *last_launch = Some(Instant::now());
        // The desktop outlives this call; a detached waiter reaps it when it exits.
        let _ = std::thread::Builder::new()
            .name("desktop-reaper".to_owned())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
    }
}

pub(super) fn run(identity: CompanionIdentity) -> Result<(), CompanionFailure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| CompanionFailure::RuntimeUnavailable)?;
    runtime.block_on(async {
        let actions = Arc::new(ActionIngress::new());
        let token = ActivationToken::default();
        let area = LinuxStatusArea::connect(
            identity,
            actions.clone(),
            token.clone(),
            None,
            None,
            HOST_GRACE,
        )
        .await?;
        let executable = std::env::current_exe()
            .ok()
            .and_then(|executable| sibling_desktop_executable(&executable));
        let dependencies = CompanionDependencies {
            status: StatusSource::Unbound,
            native: Arc::new(UnavailableNativeStatus),
            desktop: Arc::new(SiblingDesktop {
                executable,
                token,
                last_launch: Mutex::new(None),
            }),
            area: Box::new(area),
            actions,
            timer: Arc::new(TokioTimer),
        };
        CompanionController::new(dependencies).run().await
    })
}
