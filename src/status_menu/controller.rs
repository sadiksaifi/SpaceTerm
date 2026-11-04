//! The status menu's behavior, shared by every platform's status item: it follows the desktop's
//! update service and the shared Machine updates, renders after each change, and performs each
//! command, asking first wherever the update presentation says a command asks.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{App, Task};

use super::model::{Command, Menu};
use crate::ui::settings_window::local_operations::LocalCoreOperations;
use crate::updates::machines::store::{self, MachineUpdates, MachineUpdatesNotice, Surface};
use crate::updates::machines::{ActionFailure, MachineAction, MachineId, MachineUpdate};
use crate::updates::presentation::{self, DesktopAction};
use crate::updates::{UpdateService, UpdateState};

/// A native alert the status item shows by itself.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Alert {
    pub(crate) title: String,
    pub(crate) message: String,
    /// A confirmation's destructive button, such as "Restart". None for a report, which offers
    /// only OK.
    pub(crate) confirm: Option<String>,
}

/// The platform's status item.
pub(crate) trait StatusHost {
    fn render(&self, menu: &Menu);
    /// Shows `alert` and passes `answered` whether the person chose its destructive button.
    fn alert(&self, alert: Alert, answered: Box<dyn FnOnce(bool) + Send>, cx: &mut App);
    /// Opens System Settings at Login Items & Extensions, where the person turns SpaceTerm Core
    /// on. It runs on the event thread.
    fn open_login_items(&self) {
        crate::local_startup::open_login_items();
    }
    /// Reads whether SpaceTerm Core is off in Login Items. The read runs off the event thread.
    fn login_item(&self) -> crate::local_startup::LoginItemRead {
        std::sync::Arc::new(crate::local_startup::background_item_off)
    }
}

enum Message {
    /// The person closed an explanation that runs this command next.
    Answered(presentation::Command),
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
    /// Waits for the person to turn SpaceTerm Core on in Login Items after the service refused
    /// this menu's Start, then runs that Start. It reads for a few minutes only; opening the menu
    /// reads again after that.
    refused_start: RefCell<Option<Task<()>>>,
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
            refused_start: RefCell::default(),
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
            Message::Answered(command) => self.run(command, cx),
            Message::Confirmed(id, approved) => {
                let answered = self.confirmations.borrow_mut().open.remove(&id);
                if let Some(answered) = answered {
                    answered(approved, cx);
                }
            }
        }
        self.render(cx);
    }

    /// Restarts or stops on the Cores' own previews, confirming through this menu's alerts.
    fn request(&self, actions: Vec<(MachineId, MachineAction)>, always: bool, cx: &mut App) {
        let host = Rc::clone(&self.host);
        let messages = self.messages.clone();
        let confirmations = Rc::clone(&self.confirmations);
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
                confirm: Some(confirmation.button.to_string()),
            };
            let messages = messages.clone();
            host.alert(
                alert,
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
                    self.request(vec![(notice.machine, notice.action.clone())], true, cx);
                }
            }
            // The Core starts nothing that is off in Login Items, so Start opens that list,
            // where the person turns it on. The refused Start runs once the item is on, so the
            // person need not choose Start again.
            Err(ActionFailure::LoginItemOff) => {
                self.host.open_login_items();
                if notice.action == MachineAction::Start {
                    LocalCoreOperations::start_refused(cx);
                    let wait = LocalCoreOperations::start_when_login_item_on(
                        self.host.login_item(),
                        Surface::StatusMenu,
                        cx,
                    );
                    *self.refused_start.borrow_mut() = Some(wait);
                }
            }
            Err(failure) => {
                if let Some((title, message)) = presentation::action_failure(
                    &notice.name,
                    notice.local,
                    &notice.action,
                    failure,
                ) {
                    let alert = Alert {
                        title: title.to_string(),
                        message: message.to_string(),
                        confirm: None,
                    };
                    self.host.alert(alert, Box::new(|_| {}), cx);
                }
            }
        }
    }

    /// A Start, Stop or Restart of this computer's Core reaches every Workspace Window still
    /// waiting on it, as the same change made in Settings does.
    fn succeeded(&self, local: bool, action: &MachineAction, cx: &mut App) {
        if local {
            LocalCoreOperations::machine_action_succeeded(action, cx);
        }
    }

    /// The person opened the menu. A Start refused while SpaceTerm Core was off in Login Items
    /// runs now if the item is on, after the menu's own wait ended.
    pub(crate) fn opened(&self, cx: &mut App) {
        LocalCoreOperations::start_if_login_item_on(
            self.host.login_item(),
            Surface::StatusMenu,
            cx,
        );
    }

    fn render(&self, cx: &App) {
        let update = presentation::current(cx);
        let machines: &[MachineUpdate] =
            MachineUpdates::global(cx).map_or(&[], |store| store.read(cx).machines());
        let menu = Menu::new(&update, machines, crate::updates::recent_lines(cx));
        if self.menu.borrow().as_ref() == Some(&menu) {
            return;
        }
        self.host.render(&menu);
        *self.menu.borrow_mut() = Some(menu);
    }

    /// Performs `command` if the menu still offers it. A host can deliver a row it drew before
    /// the state changed.
    pub(crate) fn perform(&self, command: Command, cx: &mut App) {
        let offered = self
            .menu
            .borrow()
            .as_ref()
            .is_some_and(|menu| menu.offers(&command));
        if !offered {
            return;
        }
        match command {
            Command::Update(command) => self.run(command, cx),
            Command::Settings => {
                crate::app::leave_menu_only(cx);
                crate::ui::settings_window::open_or_activate(None, cx);
                cx.activate(true);
            }
            // The client's own quit policy decides, including its confirmation and saving.
            Command::Quit => cx.dispatch_action(&crate::app::QuitApplication),
        }
    }

    fn run(&self, command: presentation::Command, cx: &mut App) {
        match command {
            presentation::Command::Machine(
                machine,
                action @ (MachineAction::Restart | MachineAction::Stop),
            ) => self.request(vec![(machine, action)], false, cx),
            presentation::Command::Machine(machine, action) => act(machine, action, cx),
            // The confirmation is built again from the Cores' previews before it shows.
            presentation::Command::Confirm(confirmation) => {
                self.request(confirmation.actions, false, cx);
            }
            presentation::Command::Explain(explanation) => {
                let alert = Alert {
                    title: explanation.title.to_string(),
                    message: explanation.message.to_string(),
                    confirm: None,
                };
                let messages = self.messages.clone();
                let then = explanation.then;
                self.host.alert(
                    alert,
                    Box::new(move |_| {
                        if let Some(then) = then {
                            let _ = messages.send_blocking(Message::Answered(*then));
                        }
                    }),
                    cx,
                );
            }
            presentation::Command::Desktop(action) => desktop(action, cx),
        }
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
fn desktop(action: DesktopAction, cx: &mut App) {
    let service = cx
        .try_global::<UpdateService>()
        .map(|service| service.0.clone());
    match action {
        DesktopAction::Update => {
            let Some(service) = service else {
                return;
            };
            service.update(cx, |updates, cx| match updates.state() {
                UpdateState::Ready { .. } | UpdateState::Installing { .. } => updates.relaunch(cx),
                UpdateState::Available { .. } => updates.download(cx),
                _ => updates.retry_download(cx),
            });
        }
        DesktopAction::WhatsNew => {
            crate::app::activate_default_window(cx, None);
            crate::ui::updates::whats_new::open_whats_new(cx);
            cx.activate(true);
        }
        // The menu bar extra offers Retry SpaceTerm Update instead of a dismissal.
        DesktopAction::DismissFailure => {}
    }
}
