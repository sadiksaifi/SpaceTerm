//! The client's status-area item over D-Bus: a StatusNotifierItem with its com.canonical.dbusmenu
//! menu, served by the client process beside its windows. The menu is plain, as AppIndicator
//! menus are: status lines, actions that name their Machine, SpaceTerm Settings… and Quit.
//!
//! Without a status-area host the item stays on the bus and appears once a host starts. Meanwhile
//! the client reports the item unavailable, so closing its last window exits it rather than
//! leaving it unreachable. `linux_status_host` decides whether a host shows it.
use crate::status_menu::{
    self, Alert, StatusHost, StatusMenu,
    model::{Command, FlatRow, Menu as StatusMenuModel},
};
use gpui::{App, PromptButton, PromptLevel};
use std::{
    collections::HashMap,
    rc::Rc,
    sync::{Arc, Mutex, PoisonError},
};
use zbus::{
    blocking::{Connection, connection::Builder},
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

enum Message {
    Command(Command),
    /// Whether a status-area host now shows the item.
    Hosted(bool),
}

/// What the bus objects publish. The client replaces it after each change.
#[derive(Default)]
struct Published {
    summary: String,
    rows: Vec<FlatRow>,
    revision: u32,
}

type Shared = Arc<Mutex<Published>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Published> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serves this build's status item for as long as the client runs.
pub(crate) fn install(
    cx: &mut App,
    _: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
) {
    let identity = crate::application_identity::ApplicationIdentity::current();
    let shared = Arc::new(Mutex::new(Published {
        revision: 1,
        ..Published::default()
    }));
    let (sender, messages) = async_channel::unbounded();
    let connection = match Builder::session()
        .and_then(|builder| {
            builder.serve_at(
                ITEM_PATH,
                StatusItem {
                    shared: shared.clone(),
                    identity: identity.directory_name(),
                    icon: icon(identity.directory_name()),
                },
            )
        })
        .and_then(|builder| {
            builder.serve_at(
                MENU_PATH,
                Menu {
                    shared: shared.clone(),
                    sender: sender.clone(),
                },
            )
        })
        .and_then(Builder::build)
    {
        Ok(connection) => connection,
        // Without a session bus there is no status area; closing the last window exits.
        Err(_) => return,
    };
    {
        let sender = sender.clone();
        super::linux_status_host::observe(&connection, move |hosted| {
            let _ = sender.send_blocking(Message::Hosted(hosted));
        });
    }
    let item = Rc::new(LinuxStatusItem { connection, shared });
    let menu = StatusMenu::install(item, cx);
    cx.spawn(async move |cx| {
        while let Ok(message) = messages.recv().await {
            cx.update(|cx| match message {
                Message::Hosted(hosted) => status_menu::set_available(cx, hosted),
                Message::Command(command) => menu.perform(command, cx),
            });
        }
    })
    .detach();
}

/// The installed icon's absolute path, which hosts load as a file, or the application id as a
/// theme icon name. An installation directory has no icon theme index for a theme name.
fn icon(identity: &str) -> String {
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent()?.parent().map(ToOwned::to_owned))
        .map(|prefix| {
            prefix.join(format!(
                "share/icons/hicolor/scalable/apps/io.github.sadiksaifi.{identity}.svg"
            ))
        })
        .filter(|icon| icon.is_file())
        .and_then(|icon| icon.to_str().map(str::to_owned))
        .unwrap_or_else(|| format!("io.github.sadiksaifi.{identity}"))
}

struct LinuxStatusItem {
    connection: Connection,
    shared: Shared,
}

impl StatusHost for LinuxStatusItem {
    fn render(&self, presentation: &StatusMenuModel) {
        publish(&self.connection, &self.shared, presentation);
    }

    /// A status area has no alerts of its own, so the alert opens in the front Workspace window.
    fn alert(&self, alert: Alert, answered: Box<dyn FnOnce(bool) + Send>, cx: &mut App) {
        crate::app::activate_default_window(cx, None);
        let Some(handle) = crate::ui::updates::front_workspace_window(cx) else {
            return answered(false);
        };
        let (level, buttons) = match &alert.confirm {
            Some(label) => (
                PromptLevel::Critical,
                vec![
                    PromptButton::cancel("Cancel"),
                    PromptButton::ok(label.clone()),
                ],
            ),
            None => (PromptLevel::Info, vec![PromptButton::ok("OK")]),
        };
        let confirming = alert.confirm.is_some();
        let answer = handle.update(cx, |_, window, cx| {
            window.prompt(level, &alert.title, Some(&alert.message), &buttons, cx)
        });
        let Ok(answer) = answer else {
            return answered(false);
        };
        cx.spawn(async move |_| {
            let chosen = answer.await.ok();
            answered(confirming && chosen == Some(1));
        })
        .detach();
    }
}

fn publish(connection: &Connection, shared: &Shared, presentation: &StatusMenuModel) {
    let rows = presentation.flat();
    let revision = {
        let mut published = lock(shared);
        if published.rows == rows && published.summary == presentation.summary {
            return;
        }
        published.rows = rows;
        published.summary.clone_from(&presentation.summary);
        published.revision = published.revision.wrapping_add(1);
        published.revision
    };
    let core = connection.object_server();
    if let Ok(menu) = core.interface::<_, Menu>(MENU_PATH) {
        let _ = zbus::block_on(Menu::layout_updated(menu.signal_emitter(), revision, 0));
    }
    if let Ok(item) = core.interface::<_, StatusItem>(ITEM_PATH) {
        let emitter = item.signal_emitter();
        let _ = zbus::block_on(StatusItem::new_title(emitter));
        let _ = zbus::block_on(StatusItem::new_tool_tip(emitter));
        let _ = zbus::block_on(StatusItem::new_icon(emitter));
    }
}

/// StatusNotifierItem pixmaps: width, height, and ARGB32 bytes. The item draws none.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

struct StatusItem {
    shared: Shared,
    identity: &'static str,
    /// The installed icon's absolute path, or the application id as a theme icon name.
    icon: String,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl StatusItem {
    #[zbus(property)]
    fn category(&self) -> &str {
        "SystemServices"
    }

    #[zbus(property)]
    fn id(&self) -> String {
        format!("{}-core-status", self.identity)
    }

    #[zbus(property)]
    fn title(&self) -> String {
        lock(&self.shared).summary.clone()
    }

    /// Always Active: some hosts move Passive items out of sight, and a stopped Core is still
    /// something the person controls here.
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        self.icon.clone()
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
    fn attention_icon_name(&self) -> &str {
        ""
    }

    #[zbus(property)]
    fn tool_tip(&self) -> (String, Pixmaps, String, String) {
        (
            self.icon_name(),
            Vec::new(),
            lock(&self.shared).summary.clone(),
            String::new(),
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        true
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        ObjectPath::from_static_str_unchecked(MENU_PATH).into()
    }

    // The item is only a menu. Hosts that still send activation get no separate behavior.
    fn activate(&self, _x: i32, _y: i32) {}

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

/// One dbusmenu node: its id, its properties, and its children as variants of the same shape.
type Node = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

struct Menu {
    shared: Shared,
    sender: async_channel::Sender<Message>,
}

impl Menu {
    fn send(&self, message: Message) {
        let _ = self.sender.try_send(message);
    }

    /// Each row with its id, the row's index plus one. 0 is the root.
    fn rows(&self) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let published = lock(&self.shared);
        published
            .rows
            .iter()
            .zip(1..)
            .map(|(row, id)| {
                let properties = match row {
                    FlatRow::Text(text) => label(text, false),
                    FlatRow::Separator => separator(),
                    FlatRow::Action { title, .. } => label(title, true),
                };
                (id, properties)
            })
            .collect()
    }

    fn command(&self, id: i32) -> Option<Command> {
        let index = usize::try_from(id).ok()?.checked_sub(1)?;
        match lock(&self.shared).rows.get(index)? {
            FlatRow::Action { command, .. } => Some(command.clone()),
            FlatRow::Text(_) | FlatRow::Separator => None,
        }
    }
}

fn property(value: impl Into<Value<'static>>) -> OwnedValue {
    value
        .into()
        .try_into()
        .expect("menu properties hold no file descriptors")
}

fn label(text: &str, enabled: bool) -> HashMap<String, OwnedValue> {
    HashMap::from([
        // dbusmenu reads a single underscore as an access key marker.
        ("label".to_owned(), property(text.replace('_', "__"))),
        ("enabled".to_owned(), property(enabled)),
    ])
}

fn separator() -> HashMap<String, OwnedValue> {
    HashMap::from([("type".to_owned(), property("separator"))])
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
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

    fn get_layout(
        &self,
        parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Node) {
        let revision = lock(&self.shared).revision;
        let rows = self.rows();
        if parent_id != 0 {
            let properties = rows
                .into_iter()
                .find(|(id, _)| *id == parent_id)
                .map(|(_, properties)| properties)
                .unwrap_or_default();
            return (revision, (parent_id, properties, Vec::new()));
        }
        let children = rows
            .into_iter()
            .map(|(id, properties)| {
                let node: Node = (id, properties, Vec::new());
                property(Value::from(zbus::zvariant::Structure::from(node)))
            })
            .collect();
        let root = HashMap::from([("children-display".to_owned(), property("submenu"))]);
        (revision, (0, root, children))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        self.rows()
            .into_iter()
            .filter(|(id, _)| ids.is_empty() || ids.contains(id))
            .collect()
    }

    fn get_property(&self, id: i32, name: &str) -> zbus::fdo::Result<OwnedValue> {
        self.rows()
            .into_iter()
            .find(|(row, _)| *row == id)
            .and_then(|(_, mut properties)| properties.remove(name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs("unknown menu property".into()))
    }

    fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
        if event_id == "clicked"
            && let Some(command) = self.command(id)
        {
            self.send(Message::Command(command));
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event_id, data, timestamp) in events {
            self.event(id, &event_id, data, timestamp);
        }
        Vec::new()
    }

    /// The menu follows every change as it happens, so opening it needs nothing new.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}
