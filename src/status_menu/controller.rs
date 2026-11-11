//! The status menu's behavior, shared by every platform's status item: it follows the desktop's
//! update service and the shared Machine updates, renders the one update view after each change,
//! and performs each command, asking first wherever a command asks.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::App;
use spaceterm_core::service::ServiceStatus;

use super::model::{Command, Menu};
use crate::updates::UpdateService;
use crate::updates::confirm::Explanation;
use crate::updates::machines::store::{self, MachineUpdates, MachineUpdatesNotice, Surface};
use crate::updates::machines::{ActionFailure, MachineAction, MachineId, MachineUpdate};
use crate::updates::state::{self, DesktopAction};

/// A native alert the status item shows by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Alert {
    pub(crate) title: String,
    pub(crate) message: String,
    pub(crate) buttons: AlertButtons,
}

/// The buttons of an [`Alert`], and which one answers yes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AlertButtons {
    /// A report: OK only.
    Ok,
    /// Restart or Stop, which end Terminal Sessions: a caution alert with Cancel as the default
    /// button and this destructive button.
    Destructive(String),
    /// A failure's follow-up, such as Try Again or Check Again, which ends nothing: an ordinary
    /// alert with this button as the default and Cancel.
    FollowUp(String),
}

/// The platform's status item.
pub(crate) trait StatusHost {
    fn render(&self, menu: &Menu);
    /// Shows `alert` and passes `answered` whether the person chose its destructive or follow-up
    /// button.
    /// `activation` is the activation token of the menu click that led to it, if the host
    /// received one, so a window that shows the alert can come forward.
    fn alert(
        &self,
        alert: Alert,
        activation: Option<String>,
        answered: Box<dyn FnOnce(bool) + Send>,
        cx: &mut App,
    );
    /// Opens System Settings at Login Items & Extensions, where the person turns SpaceTerm Core
    /// on. It runs on the event thread.
    fn open_login_items(&self) {
        crate::local_startup::open_login_items();
    }
}

enum Message {
    /// The person closed an explanation that runs this command next.
    Answered(state::Command),
    /// The person answered the confirmation with this ID.
    Confirmed(u64, bool),
}

/// What an open confirmation does with the person's answer.
type Answered = Box<dyn FnOnce(bool, &mut App)>;

/// What each open confirmation does with its answer, by ID.
#[derive(Default)]
struct Confirmations {
    next: u64,
    open: HashMap<u64, Answered>,
}

pub(crate) struct StatusMenu {
    host: Rc<dyn StatusHost>,
    messages: async_channel::Sender<Message>,
    confirmations: Rc<RefCell<Confirmations>>,
    /// The menu as last rendered.
    menu: RefCell<Option<Menu>>,
}

impl StatusMenu {
    /// Starts the menu: it follows the desktop's update service and the shared Machine updates,
    /// and renders through `host` after every change. Install it after both services.
    pub(crate) fn install(host: Rc<dyn StatusHost>, cx: &mut App) -> Rc<Self> {
        let (messages, received) = async_channel::unbounded();
        let menu = Rc::new(Self {
            host,
            messages,
            confirmations: Rc::default(),
            menu: RefCell::default(),
        });
        if let Some(service) = cx
            .try_global::<UpdateService>()
            .map(|service| service.0.clone())
        {
            let observed = Rc::downgrade(&menu);
            cx.observe(&service, move |_, cx| {
                if let Some(menu) = observed.upgrade() {
                    menu.render(cx);
                }
            })
            .detach();
        }
        if let Some(machines) = MachineUpdates::global(cx) {
            let observed = Rc::downgrade(&menu);
            cx.observe(&machines, move |_, cx| {
                if let Some(menu) = observed.upgrade() {
                    menu.render(cx);
                }
            })
            .detach();
            let observed = Rc::downgrade(&menu);
            cx.subscribe(&machines, move |_, notice: &MachineUpdatesNotice, cx| {
                if let Some(menu) = observed.upgrade()
                    && notice.surface == Surface::StatusMenu
                {
                    menu.finished(notice, cx);
                }
            })
            .detach();
        }
        {
            let menu = Rc::clone(&menu);
            cx.spawn(async move |cx| {
                while let Ok(message) = received.recv().await {
                    cx.update(|cx| menu.receive(message, cx));
                }
            })
            .detach();
        }
        menu.render(cx);
        menu
    }

    fn receive(&self, message: Message, cx: &mut App) {
        match message {
            Message::Answered(command) => self.run(command, None, cx),
            Message::Confirmed(id, approved) => {
                let answered = self.confirmations.borrow_mut().open.remove(&id);
                if let Some(answered) = answered {
                    answered(approved, cx);
                }
            }
        }
        self.render(cx);
    }

    /// Restarts or stops on the Cores' own previews, confirming through this menu's alerts. The
    /// first alert comes forward with the click's `activation` token.
    fn request(
        &self,
        actions: Vec<(MachineId, MachineAction)>,
        always: bool,
        activation: Option<String>,
        cx: &mut App,
    ) {
        let host = Rc::clone(&self.host);
        let messages = self.messages.clone();
        let confirmations = Rc::clone(&self.confirmations);
        let activation = RefCell::new(activation);
        let ask: store::Ask = Rc::new(move |confirmation, answered, cx| {
            let id = {
                let mut confirmations = confirmations.borrow_mut();
                confirmations.next += 1;
                let id = confirmations.next;
                confirmations.open.insert(id, answered);
                id
            };
            let alert = Alert {
                title: confirmation.title.to_string(),
                message: confirmation.message.to_string(),
                buttons: AlertButtons::Destructive(confirmation.button.to_string()),
            };
            let messages = messages.clone();
            host.alert(
                alert,
                activation.take(),
                Box::new(move |approved| {
                    let _ = messages.send_blocking(Message::Confirmed(id, approved));
                }),
                cx,
            );
        });
        store::request(actions, always, Surface::StatusMenu, ask, cx);
    }

    /// An action this menu performed finished.
    fn finished(&self, notice: &MachineUpdatesNotice, cx: &mut App) {
        match notice.outcome {
            Ok(()) => self.succeeded(notice.local, &notice.action, cx),
            // The Core refused its preview: the sessions or the Pending Version changed since.
            // The menu previews again and always asks, so nothing runs again by itself.
            Err(ActionFailure::Changed) => {
                if matches!(notice.action, MachineAction::Restart | MachineAction::Stop) {
                    self.request(
                        vec![(notice.machine, notice.action.clone())],
                        true,
                        None,
                        cx,
                    );
                }
            }
            // The Core starts nothing that is off in Login Items, so Start opens that list,
            // where the person turns it on. Nothing starts by itself: once the item is on, the
            // menu still offers Start, and the person chooses it.
            Err(ActionFailure::LoginItemOff) => self.host.open_login_items(),
            Err(_) => {
                if let Some((title, message)) = notice.unasked_alert() {
                    let alert = Alert {
                        title: title.to_string(),
                        message: message.to_string(),
                        buttons: AlertButtons::Ok,
                    };
                    self.host.alert(alert, None, Box::new(|_| {}), cx);
                }
            }
        }
    }

    /// A Start, Stop or Restart of this computer's Core reaches every Workspace Window still
    /// waiting on it, as the same change made in Settings does.
    fn succeeded(&self, local: bool, action: &MachineAction, cx: &mut App) {
        let running = match action {
            MachineAction::Start | MachineAction::Restart => true,
            MachineAction::Stop => false,
            _ => return,
        };
        if local {
            super::operation_succeeded(
                cx,
                // Settings reads the service again; only `running` and `stopped` matter here.
                ServiceStatus {
                    installed: true,
                    enabled: true,
                    running,
                    stopped: !running,
                },
            );
        }
    }

    fn render(&self, cx: &App) {
        let view = state::current(cx);
        let machines: &[MachineUpdate] =
            MachineUpdates::global(cx).map_or(&[], |store| store.read(cx).machines());
        let menu = Menu::new(&view, machines);
        if self.menu.borrow().as_ref() == Some(&menu) {
            return;
        }
        self.host.render(&menu);
        *self.menu.borrow_mut() = Some(menu);
    }

    /// Performs `command` if the menu still offers it. A host can deliver a row it drew before
    /// the state changed. `activation` is the click's activation token, if the host received
    /// one: a window the command brings forward uses it.
    pub(crate) fn perform(&self, command: Command, activation: Option<String>, cx: &mut App) {
        let offered = self
            .menu
            .borrow()
            .as_ref()
            .is_some_and(|menu| menu.offers(&command));
        if !offered {
            return;
        }
        match command {
            Command::Update(command) => self.run(command, activation, cx),
            Command::Settings => {
                crate::app::leave_menu_only(cx);
                crate::ui::settings_window::open_or_activate_with(None, activation, cx);
                cx.activate(true);
            }
            // The client's own quit policy decides, including its confirmation and saving.
            Command::Quit => cx.dispatch_action(&crate::app::QuitApplication),
        }
    }

    fn run(&self, command: state::Command, activation: Option<String>, cx: &mut App) {
        match command {
            state::Command::Machine(
                machine,
                action @ (MachineAction::Restart | MachineAction::Stop),
            ) => self.request(vec![(machine, action)], false, activation, cx),
            state::Command::Machine(machine, action) => act(machine, action, cx),
            // Only the title bar opens its list.
            state::Command::ShowList => {}
            state::Command::Explain(explanation) => {
                let alert = explanation_alert(&explanation);
                let messages = self.messages.clone();
                self.host.alert(
                    alert,
                    activation,
                    Box::new(move |proceeded| {
                        if let Some(command) = explanation.answer(proceeded) {
                            let _ = messages.send_blocking(Message::Answered(command));
                        }
                    }),
                    cx,
                );
            }
            state::Command::Desktop(action) => desktop(action, activation, cx),
        }
    }
}

/// A failure's alert: its follow-up, such as Try Again, is an ordinary action beside Cancel,
/// since it ends nothing. An alert without one offers OK.
pub(super) fn explanation_alert(explanation: &Explanation) -> Alert {
    Alert {
        title: explanation.title.to_string(),
        message: explanation.message.to_string(),
        buttons: explanation.follow_up().map_or(AlertButtons::Ok, |label| {
            AlertButtons::FollowUp(label.to_string())
        }),
    }
}

/// Performs `action` through the shared Machine updates, which report back to this menu.
fn act(machine: MachineId, action: MachineAction, cx: &mut App) {
    if let Some(store) = MachineUpdates::global(cx) {
        store.update(cx, |store, cx| {
            store.perform(machine, action, Surface::StatusMenu, cx);
        });
    }
}

/// The desktop's own update. Relaunching closes no session, so it never asks.
fn desktop(action: DesktopAction, activation: Option<String>, cx: &mut App) {
    let service = cx
        .try_global::<UpdateService>()
        .map(|service| service.0.clone());
    match action {
        DesktopAction::Update => {
            let Some(service) = service else {
                return;
            };
            service.update(cx, |updates, cx| updates.relaunch(cx));
        }
        DesktopAction::Download => {
            if let Some(service) = service {
                service.update(cx, |updates, cx| updates.download_now(cx));
            }
        }
        DesktopAction::WhatsNew => {
            crate::app::activate_default_window(cx, activation.as_deref());
            crate::ui::updates::whats_new::open_whats_new(cx);
            cx.activate(true);
        }
        // The menu offers no dismissal; the title bar's failure step owns it.
        DesktopAction::DismissFailure => {}
    }
}
