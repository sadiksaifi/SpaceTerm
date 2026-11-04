//! The status menu's behavior, shared by every platform's status item: it follows the desktop's
//! update service and the shared Machine updates, renders after each change, and performs each
//! command, asking first wherever the update presentation says a command asks.
use std::cell::RefCell;
use std::rc::Rc;

use gpui::App;
use spaceterm_core::service::ServiceStatus;

use super::model::{Command, Menu};
use crate::updates::machines::store::{MachineUpdates, MachineUpdatesNotice, Surface};
use crate::updates::machines::{ActionFailure, MachineAction, MachineId};
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
}

enum Message {
    /// The person answered an alert with this command to run.
    Answered(presentation::Command),
}

pub(crate) struct StatusMenu {
    host: Rc<dyn StatusHost>,
    messages: async_channel::Sender<Message>,
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
            Message::Answered(command) => self.run(command, cx),
        }
        self.render(cx);
    }

    /// An action this menu performed finished.
    fn finished(&self, notice: &MachineUpdatesNotice, cx: &mut App) {
        match notice.outcome {
            Ok(()) => self.succeeded(notice.local, &notice.action, cx),
            // The sessions or the Pending Version changed since the person agreed, so the menu
            // asks again with what is true now.
            Err(ActionFailure::Changed) => {
                self.render(cx);
                if let Some(command) = self.command_for(notice.machine, &notice.action) {
                    self.run(command, cx);
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

    /// The command the menu now offers for `action` on `machine`.
    fn command_for(
        &self,
        machine: MachineId,
        action: &MachineAction,
    ) -> Option<presentation::Command> {
        let menu = self.menu.borrow();
        menu.as_ref()?.commands().find_map(|command| match command {
            Command::Update(presentation::Command::Machine(id, offered))
                if id == machine && offered == *action =>
            {
                Some(presentation::Command::Machine(id, offered))
            }
            Command::Update(presentation::Command::Confirm(confirmation))
                if confirmation.actions.contains(&(machine, action.clone())) =>
            {
                Some(presentation::Command::Confirm(confirmation))
            }
            _ => None,
        })
    }

    fn render(&self, cx: &App) {
        let update = presentation::current(cx);
        let (machines, recent) =
            MachineUpdates::global(cx).map_or_else(Default::default, |store| {
                let store = store.read(cx);
                (
                    store.machines().to_vec(),
                    store.recent().map(|entry| entry.text.to_string()).collect(),
                )
            });
        let menu = Menu::new(&update, &machines, recent);
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
            presentation::Command::Machine(machine, action) => act(machine, action, cx),
            presentation::Command::Confirm(confirmation) => {
                let alert = Alert {
                    title: confirmation.title.to_string(),
                    message: confirmation.message.to_string(),
                    confirm: Some(confirmation.button.to_string()),
                };
                let messages = self.messages.clone();
                let actions = confirmation.actions;
                self.host.alert(
                    alert,
                    Box::new(move |approved| {
                        if !approved {
                            return;
                        }
                        for (machine, action) in actions {
                            let command = presentation::Command::Machine(machine, action);
                            let _ = messages.send_blocking(Message::Answered(command));
                        }
                    }),
                    cx,
                );
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
