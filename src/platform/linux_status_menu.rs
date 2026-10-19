//! The client's status-area item over D-Bus: a StatusNotifierItem with its com.canonical.dbusmenu
//! menu, served by the client process beside its windows. Stop, Restart, and startup
//! authorization open the server control window in this process, without a Workspace Window.
//!
//! Without a status-area host the item stays on the bus and appears once a host starts. Meanwhile
//! the client reports the item unavailable, so closing its last window exits it rather than
//! leaving it unreachable. `linux_status_host` decides whether a host shows it.
use crate::status_menu::{
    self,
    model::{Command, Item, Presentation, State},
    worker::{Event, Request, Worker},
};
use crate::ui::server_control_window::{self, Operation, Step};
use gpui::{App, AppContext as _};
use spaceterm_server::{
    ServerError,
    service::{
        Service,
        control::{Action, Controller},
    },
};
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
    Worker(Event),
    Command(Command),
    /// Whether a status-area host now shows the item.
    Hosted(bool),
    /// The person opened the menu.
    Refresh,
}

/// What the bus objects publish. The client replaces it after each change.
struct Published {
    presentation: Presentation,
    revision: u32,
}

type Shared = Arc<Mutex<Published>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Published> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Serves this build's status item for as long as the client runs.
pub(crate) fn install(
    cx: &mut App,
    window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
) {
    let identity = crate::application_identity::ApplicationIdentity::current();
    let Ok(service) = Service::for_identity(identity.directory_name()) else {
        return;
    };
    let controller = Controller::new(service);
    let name = controller.display_name().to_owned();
    let shared = Arc::new(Mutex::new(Published {
        presentation: Presentation::of(&name, State::default()),
        revision: 1,
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
    let worker = {
        let sender = sender.clone();
        match Worker::spawn(controller.clone(), move |event| {
            let _ = sender.send_blocking(Message::Worker(event));
        }) {
            Ok(worker) => worker,
            Err(_) => return,
        }
    };
    {
        let sender = sender.clone();
        super::linux_status_host::observe(&connection, move |hosted| {
            let _ = sender.send_blocking(Message::Hosted(hosted));
        });
    }
    worker.send(Request::Refresh);
    let item = Rc::new(LinuxStatusItem {
        name,
        controller,
        worker,
        connection,
        shared,
        window_movement,
        state: std::cell::Cell::new(State::default()),
    });
    cx.spawn(async move |cx| {
        while let Ok(message) = messages.recv().await {
            let item = Rc::clone(&item);
            cx.update(|cx| item.handle(message, cx));
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
    name: String,
    controller: Controller,
    worker: Worker,
    connection: Connection,
    shared: Shared,
    window_movement: Rc<dyn crate::platform::window_movement::WindowMovementFactory>,
    state: std::cell::Cell<State>,
}

impl LinuxStatusItem {
    fn handle(self: Rc<Self>, message: Message, cx: &mut App) {
        match message {
            Message::Refresh => self.worker.send(Request::Refresh),
            Message::Hosted(hosted) => status_menu::set_available(cx, hosted),
            Message::Worker(Event::Status(status)) => self.change(|state| {
                state.status = Some(status);
            }),
            Message::Worker(Event::Finished(command, result)) => {
                self.change(|state| {
                    state.working = None;
                    if let Ok(status) = result {
                        state.status = Some(Ok(status));
                    }
                });
                status_menu::end_operation(cx);
                // Only the server control window explains and requests startup
                // authorization, and it reports every other refusal as well.
                let operation = match command {
                    Command::Stop => Operation::Stop,
                    Command::Restart => Operation::Restart,
                    _ => Operation::Start,
                };
                match result {
                    Ok(status) => status_menu::operation_succeeded(cx, status),
                    Err(ServerError::AuthorizationRequired) => {
                        self.open_control(
                            operation,
                            Step::Authorize {
                                declined: false,
                                stopped: false,
                            },
                            cx,
                        );
                    }
                    Err(error) => self.open_control(operation, Step::Failed(error), cx),
                }
            }
            Message::Command(command) => {
                let offered = {
                    let published = lock(&self.shared);
                    std::iter::once(&published.presentation.open)
                        .chain(&published.presentation.controls)
                        .chain(std::iter::once(&published.presentation.quit))
                        .find(|item| item.command == command)
                        .is_some_and(|item| item.enabled)
                };
                // A host can deliver a click for a row that changed after it drew the menu.
                if !offered {
                    return;
                }
                match command {
                    Command::Open => crate::app::activate_default_window(cx, None),
                    Command::Start => {
                        self.change(|state| state.working = Some(Command::Start));
                        status_menu::begin_operation(cx);
                        self.worker.send(Request::Start);
                    }
                    Command::Stop => self.prepare(Operation::Stop, Action::Stop, cx),
                    Command::Restart => self.prepare(Operation::Restart, Action::Restart, cx),
                    // The client's own quit policy decides, including its confirmation.
                    Command::Quit => cx.dispatch_action(&crate::app::QuitApplication),
                }
            }
        }
    }

    /// Applies one change to the item's state and publishes the result.
    fn change(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.state.get();
        change(&mut state);
        self.state.set(state);
        self.publish();
    }

    /// Reads the service off the event thread, then asks in the server control window.
    fn prepare(self: &Rc<Self>, operation: Operation, action: Action, cx: &App) {
        self.set_asking(true);
        let controller = self.controller.clone();
        let prepared = cx.background_spawn(async move { controller.prepare(action) });
        let item = Rc::clone(self);
        cx.spawn(async move |cx| {
            let step = match prepared.await {
                Ok(confirmation) => Step::Ask(confirmation),
                Err(error) => Step::Failed(error),
            };
            cx.update(|cx| item.open_control(operation, step, cx));
        })
        .detach();
    }

    fn open_control(self: &Rc<Self>, operation: Operation, step: Step, cx: &mut App) {
        self.set_asking(true);
        let item = Rc::clone(self);
        let opened = server_control_window::open(
            self.controller.clone(),
            operation,
            step,
            Rc::clone(&self.window_movement),
            Box::new(move |_: &mut App| {
                item.set_asking(false);
                item.worker.send(Request::Refresh);
            }),
            cx,
        );
        if opened.is_err() {
            self.set_asking(false);
        }
    }

    /// A server control window opened or closed.
    fn set_asking(&self, asking: bool) {
        self.change(|state| state.asking = asking);
    }

    fn publish(&self) {
        publish(
            &self.connection,
            &self.shared,
            Presentation::of(&self.name, self.state.get()),
        );
    }
}

fn publish(connection: &Connection, shared: &Shared, presentation: Presentation) {
    let revision = {
        let mut published = lock(shared);
        if published.presentation == presentation {
            return;
        }
        published.presentation = presentation;
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
        format!("{}-server-status", self.identity)
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
const QUIT: i32 = 11;
const SEPARATORS: [i32; 3] = [4, 6, 10];

fn command(id: i32) -> Option<Command> {
    Some(match id {
        OPEN => Command::Open,
        START => Command::Start,
        STOP => Command::Stop,
        RESTART => Command::Restart,
        QUIT => Command::Quit,
        _ => return None,
    })
}

struct Menu {
    shared: Shared,
    sender: async_channel::Sender<Message>,
}

impl Menu {
    fn send(&self, message: Message) {
        let _ = self.sender.try_send(message);
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
        let item = |item: &Item| label(&item.label, item.enabled);
        let [start, stop, restart] = &presentation.controls;
        rows.extend([
            (SEPARATORS[0], separator()),
            (OPEN, item(&presentation.open)),
            (SEPARATORS[1], separator()),
            (START, item(start)),
            (STOP, item(stop)),
            (RESTART, item(restart)),
            (SEPARATORS[2], separator()),
            (QUIT, item(&presentation.quit)),
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
