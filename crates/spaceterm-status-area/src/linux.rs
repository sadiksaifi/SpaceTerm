//! The status-area item over D-Bus: a StatusNotifierItem with its com.canonical.dbusmenu menu.
//! Confirmations and startup authorization open the desktop's control-only window, so they look
//! and read like every other SpaceTerm alert without opening a Workspace Window.
use crate::{
    IDENTITY, StatusError,
    linux_registration::hide,
    menu::{Command, Item, Presentation, State},
    worker::{Event, Request, Worker},
};
use spaceterm_server::{ServerError, service::control::Controller};
use std::{
    collections::HashMap,
    os::unix::process::CommandExt,
    path::Path,
    process::Stdio,
    sync::{Arc, Mutex, PoisonError, mpsc},
};
use zbus::{
    blocking::{Connection, connection::Builder},
    object_server::SignalEmitter,
    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value},
};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

enum Message {
    Worker(Event),
    Command(Command),
    /// A status-area host appeared.
    WatcherAppeared,
    /// The person opened the menu.
    Refresh,
    ControlClosed,
}

/// What the bus objects publish. The main loop replaces it after each change.
struct Published {
    presentation: Presentation,
    /// Server operations are withheld while a control window asks or works.
    controlling: bool,
    revision: u32,
}

type Shared = Arc<Mutex<Published>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Published> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs until the person hides the item or the session ends. Without a status-area host the item
/// stays on the bus, shows nothing, and appears once a host starts.
pub fn run(controller: Controller) -> Result<(), StatusError> {
    let name = controller.display_name().to_owned();
    let directory = std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_owned));
    let desktop = directory
        .as_ref()
        .map(|directory| directory.join("spaceterm"))
        .filter(|desktop| desktop.is_file());
    // Hosts load an absolute IconName as a file. A theme name needs an icon theme index, which
    // an installation directory does not have.
    let icon = directory
        .as_ref()
        .and_then(|directory| directory.parent())
        .map(|prefix| {
            prefix.join(format!(
                "share/icons/hicolor/scalable/apps/io.github.sadiksaifi.{IDENTITY}.svg"
            ))
        })
        .filter(|icon| icon.is_file())
        .and_then(|icon| icon.to_str().map(str::to_owned))
        .unwrap_or_else(|| format!("io.github.sadiksaifi.{IDENTITY}"));
    let mut state = State::default();
    let shared = Arc::new(Mutex::new(Published {
        presentation: Presentation::of(&name, state, desktop.is_some()),
        controlling: false,
        revision: 1,
    }));
    let (sender, messages) = mpsc::channel();
    let bus_name = format!("io.github.sadiksaifi.{IDENTITY}.StatusArea");
    let connection = match Builder::session()
        .and_then(|builder| builder.name(bus_name.as_str()))
        .and_then(|builder| {
            builder.serve_at(
                ITEM_PATH,
                StatusItem {
                    shared: shared.clone(),
                    icon,
                },
            )
        })
        .and_then(|builder| {
            builder.serve_at(
                MENU_PATH,
                Menu {
                    shared: shared.clone(),
                    sender: Mutex::new(sender.clone()),
                },
            )
        })
        .and_then(Builder::build)
    {
        Ok(connection) => connection,
        // Another status item for this build already runs in the session.
        Err(zbus::Error::NameTaken) => return Ok(()),
        Err(_) => return Err(StatusError::SessionUnavailable),
    };
    watch_hosts(&connection, sender.clone());
    let worker = {
        let sender = sender.clone();
        Worker::spawn(controller, move |event| {
            let _ = sender.send(Message::Worker(event));
        })?
    };
    register_item(&connection);
    worker.send(Request::Refresh);
    let mut controlling = false;
    for message in messages {
        match message {
            Message::Worker(Event::Status(status)) => state.status = Some(status),
            Message::Worker(Event::Finished(command, result)) => {
                state.working = None;
                match result {
                    Ok(status) => state.status = Some(Ok(status)),
                    // Only the control window explains and requests startup authorization.
                    Err(ServerError::AuthorizationRequired) if desktop.is_some() => {
                        controlling = open_control(desktop.as_deref(), "start", &sender);
                    }
                    Err(error) => notify(&connection, &name, command, error),
                }
            }
            Message::Command(command) => {
                let published = lock(&shared);
                let item = std::iter::once(&published.presentation.open)
                    .chain(&published.presentation.controls)
                    .chain(std::iter::once(&published.presentation.hide))
                    .find(|item| item.command == command);
                // A host can deliver a click for a row that changed after it drew the menu.
                if !item.is_some_and(|item| enabled(item, published.controlling)) {
                    continue;
                }
                drop(published);
                match command {
                    Command::Open => {
                        if let Some(desktop) = &desktop {
                            launch(std::process::Command::new(desktop), || {});
                        }
                    }
                    Command::Start => {
                        state.working = Some(Command::Start);
                        worker.send(Request::Start);
                    }
                    Command::Stop => {
                        controlling = open_control(desktop.as_deref(), "stop", &sender);
                    }
                    Command::Restart => {
                        controlling = open_control(desktop.as_deref(), "restart", &sender);
                    }
                    Command::Hide => {
                        hide(IDENTITY);
                        return Ok(());
                    }
                }
            }
            Message::WatcherAppeared => {
                register_item(&connection);
                continue;
            }
            Message::Refresh => {
                worker.send(Request::Refresh);
                continue;
            }
            Message::ControlClosed => {
                controlling = false;
                worker.send(Request::Refresh);
            }
        }
        publish(
            &connection,
            &shared,
            Presentation::of(&name, state, desktop.is_some()),
            controlling,
        );
    }
    Ok(())
}

fn enabled(item: &Item, controlling: bool) -> bool {
    item.enabled
        && !(controlling
            && matches!(
                item.command,
                Command::Start | Command::Stop | Command::Restart
            ))
}

/// Opens the desktop's control-only window for one operation and reports when it closes. The
/// window asks, authorizes, and runs the operation itself, so hiding this item cannot abandon it.
fn open_control(desktop: Option<&Path>, action: &str, sender: &mpsc::Sender<Message>) -> bool {
    let Some(desktop) = desktop else {
        return false;
    };
    let mut command = std::process::Command::new(desktop);
    command.args(["--server-control", action]);
    let sender = sender.clone();
    launch(command, move || {
        let _ = sender.send(Message::ControlClosed);
    })
    .is_some()
}

/// Starts a process in its own session, so it outlives this item, and reaps it when it exits so
/// a long-lived item collects no zombies. `exited` runs after the process ended. Returns the
/// process id, or `None` when it could not start.
fn launch(
    mut command: std::process::Command,
    exited: impl FnOnce() + Send + 'static,
) -> Option<u32> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches no memory shared with the parent.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().ok()?;
    let id = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
        exited();
    });
    Some(id)
}

/// Tells the session's status-area host about the item. Without a host this does nothing.
///
/// The item registers by this connection's unique name, not the build's well-known name. A
/// previous instance registered that well-known name too, and a host that processes its exit
/// after this registration would otherwise remove the new item along with the old one.
fn register_item(connection: &Connection) {
    let Some(unique) = connection.unique_name() else {
        return;
    };
    let _ = connection.call_method(
        Some(WATCHER),
        "/StatusNotifierWatcher",
        Some(WATCHER),
        "RegisterStatusNotifierItem",
        &(unique.as_str(),),
    );
}

/// Registers again whenever a status-area host starts, such as after the shell restarts.
fn watch_hosts(connection: &Connection, sender: mpsc::Sender<Message>) {
    let Ok(proxy) = zbus::blocking::fdo::DBusProxy::new(connection) else {
        return;
    };
    let Ok(changes) = proxy.receive_name_owner_changed_with_args(&[(0, WATCHER)]) else {
        return;
    };
    std::thread::spawn(move || {
        for change in changes {
            if change.args().is_ok_and(|args| args.new_owner().is_some())
                && sender.send(Message::WatcherAppeared).is_err()
            {
                return;
            }
        }
    });
}

fn publish(
    connection: &Connection,
    shared: &Shared,
    presentation: Presentation,
    controlling: bool,
) {
    let revision = {
        let mut published = lock(shared);
        if published.presentation == presentation && published.controlling == controlling {
            return;
        }
        published.presentation = presentation;
        published.controlling = controlling;
        published.revision = published.revision.wrapping_add(1);
        published.revision
    };
    let server = connection.object_server();
    if let Ok(menu) = server.interface::<_, Menu>(MENU_PATH) {
        let _ = zbus::block_on(Menu::layout_updated(menu.signal_emitter(), revision, 0));
    }
    if let Ok(item) = server.interface::<_, StatusItem>(ITEM_PATH) {
        let emitter = item.signal_emitter();
        let _ = zbus::block_on(StatusItem::new_title(emitter));
        let _ = zbus::block_on(StatusItem::new_tool_tip(emitter));
        let _ = zbus::block_on(StatusItem::new_icon(emitter));
    }
}

/// Reports a refused Start where the person will see it. Notifications carry only bounded text.
fn notify(connection: &Connection, name: &str, command: Command, error: ServerError) {
    let operation = match command {
        Command::Start => "start",
        Command::Stop => "stop",
        _ => "restart",
    };
    let summary = format!("The {name} server couldn't {operation}.");
    let hints: HashMap<&str, Value<'_>> = HashMap::new();
    let _ = connection.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &(
            format!("{name} Server Status"),
            0u32,
            format!("io.github.sadiksaifi.{IDENTITY}"),
            summary,
            error.to_string(),
            Vec::<&str>::new(),
            hints,
            -1i32,
        ),
    );
}

/// StatusNotifierItem pixmaps: width, height, and ARGB32 bytes. The item draws none.
type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

struct StatusItem {
    shared: Shared,
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
        format!("{IDENTITY}-server-status")
    }

    #[zbus(property)]
    fn title(&self) -> String {
        lock(&self.shared).presentation.summary.clone()
    }

    /// Always Active: some hosts move Passive items out of sight, and a stopped server is still
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
        let published = lock(&self.shared);
        (
            self.icon_name(),
            Vec::new(),
            published.presentation.summary.clone(),
            published
                .presentation
                .explanation
                .clone()
                .unwrap_or_default(),
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

/// Row ids. 0 is the root.
const HEADER: i32 = 1;
const STATE: i32 = 2;
const EXPLANATION: i32 = 3;
const OPEN: i32 = 5;
const START: i32 = 7;
const STOP: i32 = 8;
const RESTART: i32 = 9;
const HIDE: i32 = 11;
const SEPARATORS: [i32; 3] = [4, 6, 10];

fn command(id: i32) -> Option<Command> {
    Some(match id {
        OPEN => Command::Open,
        START => Command::Start,
        STOP => Command::Stop,
        RESTART => Command::Restart,
        HIDE => Command::Hide,
        _ => return None,
    })
}

struct Menu {
    shared: Shared,
    sender: Mutex<mpsc::Sender<Message>>,
}

impl Menu {
    fn send(&self, message: Message) {
        let _ = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(message);
    }

    fn rows(&self) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let published = lock(&self.shared);
        let presentation = &published.presentation;
        let mut rows = vec![
            (HEADER, label(&presentation.server, false)),
            (STATE, label(presentation.state, false)),
        ];
        if let Some(explanation) = &presentation.explanation {
            rows.push((EXPLANATION, label(explanation, false)));
        }
        let item = |item: &Item| label(&item.label, enabled(item, published.controlling));
        let [start, stop, restart] = &presentation.controls;
        rows.extend([
            (SEPARATORS[0], separator()),
            (OPEN, item(&presentation.open)),
            (SEPARATORS[1], separator()),
            (START, item(start)),
            (STOP, item(stop)),
            (RESTART, item(restart)),
            (SEPARATORS[2], separator()),
            (HIDE, item(&presentation.hide)),
        ]);
        rows
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
            && let Some(command) = command(id)
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

    /// Opening the menu reads the current state. Reading never changes the service.
    fn about_to_show(&self, id: i32) -> bool {
        if id == 0 {
            self.send(Message::Refresh);
        }
        false
    }

    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        if ids.contains(&0) {
            self.send(Message::Refresh);
        }
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launched_process_is_reaped_after_it_exits() {
        let (reaped, exited) = mpsc::channel();
        let id = launch(std::process::Command::new("/bin/true"), move || {
            let _ = reaped.send(());
        })
        .expect("the process starts");
        exited
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the process exits");
        // A process that exited but was never waited for stays in /proc as a zombie.
        assert!(!Path::new(&format!("/proc/{id}")).exists());
    }
}
