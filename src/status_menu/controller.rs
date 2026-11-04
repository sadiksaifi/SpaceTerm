//! The status menu's behavior, shared by every platform's status item: it follows the desktop's
//! update service and every Machine, renders after each change, and performs each command,
//! asking first wherever the update presentation says a command asks.
use std::cell::RefCell;
use std::rc::Rc;

use gpui::App;
use spaceterm_core::service::ServiceStatus;

use super::model::{Command, Menu};
use crate::updates::machines::{
    ActionFailure, MachineAction, MachineId, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::presentation::{self, DesktopAction, DesktopFacts, Dismissed, Presentation};
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
    Machines(MachineUpdatesEvent),
    /// The person answered an alert with this command to run.
    Answered(presentation::Command),
}

pub(crate) struct StatusMenu {
    host: Rc<dyn StatusHost>,
    adapter: Option<Rc<dyn MachineUpdatesAdapter>>,
    messages: async_channel::Sender<Message>,
    state: RefCell<State>,
}

#[derive(Default)]
struct State {
    machines: Vec<MachineUpdate>,
    dismissed: Dismissed,
    /// Recent history, newest first.
    recent: Vec<String>,
    menu: Option<Menu>,
    /// Actions this menu performed that have not finished, so it reports only its own failures.
    performed: Vec<(MachineId, MachineAction)>,
}

impl StatusMenu {
    /// Starts the menu: it follows the desktop's update service and `adapter`'s Machines, and
    /// renders through `host` after every change.
    pub(crate) fn install(
        host: Rc<dyn StatusHost>,
        adapter: Option<Rc<dyn MachineUpdatesAdapter>>,
        cx: &mut App,
    ) -> Rc<Self> {
        let (messages, received) = async_channel::unbounded();
        if let Some(adapter) = &adapter {
            let (events, machine_events) = async_channel::unbounded();
            if adapter.start(events).is_ok() {
                let messages = messages.clone();
                cx.spawn(async move |_| {
                    while let Ok(event) = machine_events.recv().await {
                        if messages.send(Message::Machines(event)).await.is_err() {
                            return;
                        }
                    }
                })
                .detach();
            }
        }
        let menu = Rc::new(Self {
            host,
            adapter,
            messages,
            state: RefCell::default(),
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
            Message::Machines(event) => self.machines_changed(event, cx),
            Message::Answered(command) => self.run(command, cx),
        }
        self.render(cx);
    }

    fn machines_changed(&self, event: MachineUpdatesEvent, cx: &mut App) {
        match event {
            MachineUpdatesEvent::Machines(machines) => self.state.borrow_mut().machines = machines,
            MachineUpdatesEvent::Changed(update) => {
                let mut state = self.state.borrow_mut();
                match state
                    .machines
                    .iter_mut()
                    .find(|machine| machine.machine == update.machine)
                {
                    Some(machine) => *machine = *update,
                    None => state.machines.push(*update),
                }
            }
            MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome,
            } => {
                let ours = {
                    let mut state = self.state.borrow_mut();
                    let index = state
                        .performed
                        .iter()
                        .position(|performed| *performed == (machine, action.clone()));
                    index.map(|index| state.performed.remove(index)).is_some()
                };
                match outcome {
                    Err(_) if !ours => {}
                    Ok(()) => self.succeeded(machine, &action, cx),
                    // The sessions or the Pending Version changed since the person agreed, so
                    // the menu asks again with what is true now.
                    Err(ActionFailure::Changed) => {
                        self.render(cx);
                        if let Some(command) = self.command_for(machine, &action) {
                            self.run(command, cx);
                        }
                    }
                    Err(failure) => self.report(machine, &action, failure, cx),
                }
            }
        }
    }

    /// A Start, Stop or Restart of this computer's Core reaches every Workspace Window still
    /// waiting on it, as the same change made in Settings does.
    fn succeeded(&self, machine: MachineId, action: &MachineAction, cx: &mut App) {
        let local = self
            .state
            .borrow()
            .machines
            .iter()
            .any(|update| update.machine == machine && update.local);
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
        let state = self.state.borrow();
        state
            .menu
            .as_ref()?
            .commands()
            .find_map(|command| match command {
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
        let menu = {
            let state = self.state.borrow();
            let service = cx
                .try_global::<UpdateService>()
                .map(|service| service.0.read(cx));
            let unavailable = UpdateState::Unavailable;
            let desktop = DesktopFacts {
                state: service.map_or(&unavailable, |updates| updates.state()),
                pending: service.and_then(|updates| updates.pending_version()),
                stage: service
                    .and_then(|updates| updates.reminder())
                    .unwrap_or_default(),
                notes_unread: crate::ui::updates::whats_new::release_notes_unread(cx),
                running: env!("SPACETERM_VERSION"),
            };
            let update = Presentation::new(
                desktop,
                &state.machines,
                &state.dismissed,
                crate::updates::now(),
            );
            Menu::new(&update, &state.machines, state.recent.clone())
        };
        if self.state.borrow().menu.as_ref() == Some(&menu) {
            return;
        }
        self.host.render(&menu);
        self.state.borrow_mut().menu = Some(menu);
    }

    /// Performs `command` if the menu still offers it. A host can deliver a row it drew before
    /// the state changed.
    pub(crate) fn perform(&self, command: Command, cx: &mut App) {
        let offered = self
            .state
            .borrow()
            .menu
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
            presentation::Command::Machine(machine, action) => self.act(machine, action),
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

    fn act(&self, machine: MachineId, action: MachineAction) {
        let Some(adapter) = &self.adapter else {
            return;
        };
        {
            let mut state = self.state.borrow_mut();
            if let MachineAction::DismissFailure(id) = action {
                state.dismissed.insert((machine, id));
            }
            state.performed.push((machine, action.clone()));
        }
        adapter.perform(machine, action);
    }

    fn report(
        &self,
        machine: MachineId,
        action: &MachineAction,
        failure: ActionFailure,
        cx: &mut App,
    ) {
        let alert = {
            let state = self.state.borrow();
            let Some(update) = state.machines.iter().find(|m| m.machine == machine) else {
                return;
            };
            failure_alert(update, action, failure)
        };
        self.host.alert(alert, Box::new(|_| {}), cx);
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
                UpdateState::Ready { .. } => {
                    if let Some(token) = updates.begin_install_confirmation() {
                        updates.finish_install_confirmation(token, true, cx);
                    }
                }
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

/// The full sentence a failed action reports, with OK.
fn failure_alert(machine: &MachineUpdate, action: &MachineAction, failure: ActionFailure) -> Alert {
    let place = match (machine.local, cfg!(target_os = "macos")) {
        (true, true) => "this Mac".to_owned(),
        (true, false) => "this computer".to_owned(),
        (false, _) => machine.name.to_string(),
    };
    let operation = match action {
        MachineAction::Install => "install the update",
        MachineAction::Restart => "restart SpaceTerm Core",
        MachineAction::CheckAgain => "check SpaceTerm Core",
        MachineAction::DismissFailure(_) => "dismiss the notice",
        MachineAction::Start => "start SpaceTerm Core",
        MachineAction::Stop => "stop SpaceTerm Core",
    };
    let message = match failure {
        ActionFailure::Unavailable(_) => {
            format!("SpaceTerm Core on {place} is too old to do this.")
        }
        ActionFailure::Busy => format!("SpaceTerm Core on {place} is busy with another change."),
        ActionFailure::Changed => format!("SpaceTerm Core on {place} changed. Try again."),
        ActionFailure::Unreachable => format!("SpaceTerm can't reach {place}."),
        ActionFailure::Failed => format!("SpaceTerm Core on {place} refused the request."),
    };
    Alert {
        title: format!("SpaceTerm couldn't {operation} on {place}."),
        message,
        confirm: None,
    }
}
