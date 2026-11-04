use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, TestAppContext};
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};

use super::model::{Command, FlatRow, Menu};
use super::{Alert, StatusHost, StatusMenu};
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::presentation::ExtraItem;

const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];

fn machine(id: MachineId, name: &'static str, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        machine: id,
        name: name.into(),
        local: id == LOCAL,
        link: MachineLink::Connected,
        facts: Some(UpdateFacts {
            revision: 1,
            platform: Platform::LinuxX86_64,
            running: Version::parse("0.8.0").unwrap(),
            pending: None,
            sessions,
            operation: None,
            check: UpdateCheck::Succeeded {
                at: crate::updates::now(),
            },
            skipped: None,
            last_failure: None,
        }),
        capabilities: Capabilities::fake(&Capability::ALL),
        push: None,
    }
}

/// `update` with 0.9.0 installed just now and waiting on a restart.
fn waiting(mut update: MachineUpdate) -> MachineUpdate {
    update.facts.as_mut().unwrap().pending = Some(PendingVersion {
        version: Version::parse("0.9.0").unwrap(),
        protocols: Protocols::CURRENT,
        breaking: false,
        installed_at: crate::updates::now(),
        overdue_since: None,
        stranded: Vec::new(),
    });
    update
}

/// How an alert hears its answer.
type Answered = Box<dyn FnOnce(bool) + Send>;

/// A status item that records each render and alert, and answers alerts as told.
#[derive(Default)]
struct Host {
    menus: RefCell<Vec<Menu>>,
    alerts: RefCell<Vec<Alert>>,
    /// Answers for the next alerts, in order. An alert with no answer left stays open.
    answers: RefCell<Vec<bool>>,
    /// The open alert's answer, until [`Host::answer`] gives it.
    open: RefCell<Option<Answered>>,
    /// How many times the menu opened Login Items.
    login_items: std::cell::Cell<u32>,
    /// Whether SpaceTerm Core is off in Login Items.
    login_item_off: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl StatusHost for Host {
    fn render(&self, menu: &Menu) {
        self.menus.borrow_mut().push(menu.clone());
    }

    fn alert(&self, alert: Alert, answered: Answered, _: &mut App) {
        self.alerts.borrow_mut().push(alert);
        let mut answers = self.answers.borrow_mut();
        if answers.is_empty() {
            *self.open.borrow_mut() = Some(answered);
        } else {
            answered(answers.remove(0));
        }
    }

    fn open_login_items(&self) {
        self.login_items.set(self.login_items.get() + 1);
    }

    fn login_item(&self) -> crate::local_startup::LoginItemRead {
        let off = std::sync::Arc::clone(&self.login_item_off);
        std::sync::Arc::new(move || off.load(std::sync::atomic::Ordering::SeqCst))
    }
}

impl Host {
    /// Answers the alert left open.
    fn answer(&self, approved: bool) {
        let answered = self.open.borrow_mut().take().expect("an alert is open");
        answered(approved);
    }

    fn menu(&self) -> Menu {
        self.menus
            .borrow()
            .last()
            .cloned()
            .expect("the menu rendered")
    }
}

struct Fixture {
    host: Rc<Host>,
    machines: Rc<ScriptedMachines>,
    menu: Rc<StatusMenu>,
}

fn start(machines: Vec<MachineUpdate>, cx: &mut TestAppContext) -> Fixture {
    let host = Rc::new(Host::default());
    let adapter = Rc::new(ScriptedMachines::default());
    let menu = cx.update(|cx| {
        crate::updates::machines::store::MachineUpdates::install(
            Rc::clone(&adapter) as Rc<dyn MachineUpdatesAdapter>,
            cx,
        );
        StatusMenu::install(Rc::clone(&host) as Rc<dyn StatusHost>, cx)
    });
    adapter.emit(MachineUpdatesEvent::Machines(machines));
    cx.run_until_parked();
    Fixture {
        host,
        machines: adapter,
        menu,
    }
}

/// The command behind the submenu item titled `title` on `machine`'s row.
fn submenu_command(menu: &Menu, machine: MachineId, title: &str) -> Command {
    let row = menu
        .machines
        .iter()
        .find(|row| row.row.machine == machine)
        .expect("the Machine has a row");
    row.row
        .items
        .iter()
        .flatten()
        .find_map(|item| match item {
            ExtraItem::Action {
                title: offered,
                command,
                ..
            } if offered.as_ref() == title => Some(Command::Update(command.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{title} is offered"))
}

fn action_command(menu: &Menu, title: &str) -> Command {
    menu.actions
        .iter()
        .find_map(|item| match item {
            ExtraItem::Action {
                title: offered,
                command,
                ..
            } if offered.as_ref() == title => Some(Command::Update(command.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{title} is offered"))
}

#[gpui::test]
fn restarting_a_core_with_sessions_asks_first_with_the_agreed_alert(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update\u{2026}");

    cx.update(|cx| fixture.menu.perform(restart.clone(), cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().as_slice(),
        [Alert {
            title: "Restart SpaceTerm Core on penguin?".to_owned(),
            message: "This will close your 3 running sessions.".to_owned(),
            confirm: Some("Restart".to_owned()),
        }]
    );
    assert!(
        fixture.machines.executed_actions().is_empty(),
        "nothing restarts while the alert is open"
    );

    fixture.host.answers.borrow_mut().push(false);
    cx.update(|cx| fixture.menu.perform(restart.clone(), cx));
    cx.run_until_parked();
    assert!(
        fixture.machines.executed_actions().is_empty(),
        "Cancel restarts nothing"
    );

    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn restarting_a_core_without_sessions_restarts_at_once(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");

    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert!(fixture.host.alerts.borrow().is_empty());
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn restart_all_asks_once_then_restarts_every_waiting_machine(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Mac", 3)),
            waiting(machine(PENGUIN, "penguin", 4)),
            machine(BUILD_BOX, "build-box", 1),
        ],
        cx,
    );
    let restart_all = action_command(&fixture.host.menu(), "Restart All\u{2026}");
    fixture.host.answers.borrow_mut().push(true);

    cx.update(|cx| fixture.menu.perform(restart_all, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().as_slice(),
        [Alert {
            title: "Restart SpaceTerm Core on 2 machines?".to_owned(),
            message: "This will close 7 running sessions.".to_owned(),
            confirm: Some("Restart".to_owned()),
        }]
    );
    assert_eq!(
        fixture.machines.executed_actions(),
        [
            (LOCAL, MachineAction::Restart),
            (PENGUIN, MachineAction::Restart)
        ]
    );
}

#[gpui::test]
fn only_this_mac_offers_stop_and_stopping_with_sessions_asks(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            machine(LOCAL, "This Mac", 2),
            machine(PENGUIN, "penguin", 2),
        ],
        cx,
    );
    let menu = fixture.host.menu();
    let stops = |machine: MachineId| {
        menu.machines
            .iter()
            .find(|row| row.row.machine == machine)
            .and_then(|row| row.row.items.clone())
            .unwrap_or_default()
            .into_iter()
            .filter(
                |item| matches!(item, ExtraItem::Action { title, .. } if title.starts_with("Stop")),
            )
            .count()
    };
    assert_eq!((stops(LOCAL), stops(PENGUIN)), (1, 0));

    let stop = submenu_command(&menu, LOCAL, "Stop SpaceTerm Core\u{2026}");
    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(stop, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow()[0].confirm.as_deref(),
        Some("Stop")
    );
    assert_eq!(
        fixture.machines.executed_actions(),
        [(LOCAL, MachineAction::Stop)]
    );
}

#[gpui::test]
fn a_restart_drawn_before_sessions_opened_never_runs_without_asking(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");
    // Someone opened 2 sessions on penguin after the menu drew its no-ask restart.
    fixture
        .machines
        .emit(MachineUpdatesEvent::Changed(Box::new(waiting(machine(
            PENGUIN, "penguin", 2,
        )))));
    cx.run_until_parked();

    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert!(fixture.machines.executed_actions().is_empty());
    assert!(fixture.host.alerts.borrow().is_empty());
}

#[gpui::test]
fn a_failed_action_from_the_menu_opens_an_alert_and_others_stay_quiet(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    // An action no surface performed fails too; nobody reports it.
    for _ in 0..2 {
        fixture.machines.emit(MachineUpdatesEvent::Finished {
            machine: PENGUIN,
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Unreachable),
        });
    }
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().as_slice(),
        [Alert {
            title: "Couldn't restart SpaceTerm Core on penguin".to_owned(),
            message: "SpaceTerm can't reach penguin. Try again when it is connected.".to_owned(),
            confirm: None,
        }]
    );
}

#[gpui::test]
fn the_menu_ends_with_settings_and_quit_and_never_offers_open(cx: &mut TestAppContext) {
    let fixture = start(vec![machine(PENGUIN, "penguin", 3)], cx);
    let menu = fixture.host.menu();
    let commands: Vec<Command> = menu.commands().collect();
    assert_eq!(
        commands[commands.len() - 2..],
        [Command::Settings, Command::Quit]
    );
    assert_eq!(
        menu.header.line.as_ref(),
        format!("Version {}", env!("SPACETERM_VERSION"))
    );
    assert_eq!(
        menu.machines[0].row.line.as_ref(),
        "Version 0.8.0 \u{b7} 3 sessions"
    );
    assert!(
        menu.actions.is_empty(),
        "nothing pending offers no actions group"
    );
}

#[gpui::test]
fn the_flat_menu_names_the_machine_each_pending_action_affects(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            machine(LOCAL, "This Mac", 0),
            waiting(machine(PENGUIN, "penguin", 3)),
            waiting(machine(BUILD_BOX, "build-box", 1)),
        ],
        cx,
    );
    let flat = fixture.host.menu().flat();
    let rows: Vec<String> = flat
        .iter()
        .map(|row| match row {
            FlatRow::Text(text) => text.clone(),
            FlatRow::Separator => "-".to_owned(),
            FlatRow::Action { title, .. } => format!("[{title}]"),
        })
        .collect();
    assert_eq!(
        rows,
        [
            format!("SpaceTerm \u{b7} Version {}", env!("SPACETERM_VERSION")),
            "This Mac \u{b7} Version 0.8.0 \u{b7} No sessions".to_owned(),
            "penguin \u{b7} Version 0.8.0 \u{b7} Update ready".to_owned(),
            "build-box \u{b7} Version 0.8.0 \u{b7} Update ready".to_owned(),
            "-".to_owned(),
            "[Restart SpaceTerm Core on This Mac]".to_owned(),
            "[Stop SpaceTerm Core on This Mac]".to_owned(),
            "[Restart to Update on penguin\u{2026}]".to_owned(),
            "[Restart to Update on build-box\u{2026}]".to_owned(),
            "[Restart All\u{2026}]".to_owned(),
            "-".to_owned(),
            "[SpaceTerm Settings\u{2026}]".to_owned(),
            "[Quit SpaceTerm]".to_owned(),
        ]
    );
    // Every flat action is one the menu offers.
    let menu = fixture.host.menu();
    for row in &flat {
        if let FlatRow::Action { command, .. } = row {
            assert!(menu.offers(command), "{command:?}");
        }
    }
}

#[gpui::test]
fn updating_the_desktop_relaunches_without_asking(cx: &mut TestAppContext) {
    use crate::updates::testing::RecordingAdapter;
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let updater = Rc::new(RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
    service.update(cx, |updates, cx| updates.check(false, cx));
    updater.emit(UpdateEvent::Available("0.9.0".into()));
    cx.run_until_parked();
    service.update(cx, |updates, cx| updates.download(cx));
    updater.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    let fixture = start(vec![machine(PENGUIN, "penguin", 3)], cx);
    let menu = fixture.host.menu();
    assert!(
        menu.header.line.ends_with("Update ready"),
        "{}",
        menu.header.line
    );

    let relaunch = action_command(&menu, "Update SpaceTerm and Relaunch");
    cx.update(|cx| fixture.menu.perform(relaunch, cx));
    cx.run_until_parked();
    assert!(fixture.host.alerts.borrow().is_empty());
    assert_eq!(updater.installations.get(), 1);
}

/// `update` with its sessions set to `sessions`.
fn with_sessions(mut update: MachineUpdate, sessions: u32) -> MachineUpdate {
    update.facts.as_mut().unwrap().sessions = sessions;
    update
}

fn changed(fixture: &Fixture, update: MachineUpdate, cx: &mut TestAppContext) {
    fixture
        .machines
        .emit(MachineUpdatesEvent::Changed(Box::new(update)));
    cx.run_until_parked();
}

#[gpui::test]
fn sessions_that_change_while_the_alert_is_open_ask_again(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    changed(&fixture, waiting(machine(PENGUIN, "penguin", 4)), cx);

    fixture.host.answer(true);
    cx.run_until_parked();
    assert!(
        fixture.machines.executed_actions().is_empty(),
        "the approval covered 3 sessions, not 4"
    );
    assert_eq!(
        fixture.host.alerts.borrow().last().unwrap().message,
        "This will close your 4 running sessions."
    );

    fixture.host.answer(true);
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn a_new_pending_version_while_the_alert_is_open_asks_again(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    let mut newer = waiting(machine(PENGUIN, "penguin", 3));
    newer
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .version = Version::parse("0.9.1").unwrap();
    changed(&fixture, newer, cx);

    fixture.host.answer(true);
    cx.run_until_parked();
    assert!(fixture.machines.executed_actions().is_empty());
    assert_eq!(fixture.host.alerts.borrow().len(), 2, "it asks again");
    fixture.host.answer(true);
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn sessions_that_all_close_while_the_alert_is_open_restart_at_once(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    changed(&fixture, waiting(machine(PENGUIN, "penguin", 0)), cx);

    fixture.host.answer(true);
    cx.run_until_parked();
    assert_eq!(fixture.host.alerts.borrow().len(), 1);
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
}

#[gpui::test]
fn restart_all_and_stop_ask_again_after_their_sessions_change(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Mac", 3)),
            waiting(machine(PENGUIN, "penguin", 4)),
        ],
        cx,
    );
    let restart_all = action_command(&fixture.host.menu(), "Restart All\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart_all, cx));
    cx.run_until_parked();
    changed(
        &fixture,
        with_sessions(waiting(machine(PENGUIN, "penguin", 4)), 5),
        cx,
    );
    fixture.host.answer(true);
    cx.run_until_parked();
    assert!(fixture.machines.executed_actions().is_empty());
    assert_eq!(
        fixture.host.alerts.borrow().last().unwrap().message,
        "This will close 8 running sessions."
    );
    fixture.host.answer(false);
    cx.run_until_parked();

    let stop = submenu_command(&fixture.host.menu(), LOCAL, "Stop SpaceTerm Core\u{2026}");
    cx.update(|cx| fixture.menu.perform(stop, cx));
    cx.run_until_parked();
    changed(
        &fixture,
        with_sessions(waiting(machine(LOCAL, "This Mac", 3)), 1),
        cx,
    );
    fixture.host.answer(true);
    cx.run_until_parked();
    assert!(fixture.machines.executed_actions().is_empty());
    assert_eq!(
        fixture.host.alerts.borrow().last().unwrap().message,
        "This will close your 1 running session. SpaceTerm Core stays stopped until you start it again."
    );
}

#[gpui::test]
fn a_finished_core_update_reaches_recent_updates(cx: &mut TestAppContext) {
    use crate::updates::ApplicationUpdates;
    use crate::updates::testing::RecordingAdapter;
    let updater = Rc::new(RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    assert!(fixture.host.menu().recent.is_empty());

    let mut updated = machine(PENGUIN, "penguin", 0);
    updated.facts.as_mut().unwrap().running = Version::parse("0.9.0").unwrap();
    changed(&fixture, updated, cx);
    let recent = fixture.host.menu().recent;
    assert_eq!(recent.len(), 1, "{recent:?}");
    assert!(
        recent[0].starts_with("penguin updated to 0.9.0 \u{b7} "),
        "{recent:?}"
    );
    let saved = updater
        .saved
        .borrow()
        .clone()
        .expect("the history was saved");
    assert_eq!(saved.recent()[0].text, "penguin updated to 0.9.0");
}

#[gpui::test]
fn the_flat_menu_offers_start_for_a_stopped_local_core(cx: &mut TestAppContext) {
    let mut stopped = machine(LOCAL, "This Computer", 0);
    stopped.link = MachineLink::Stopped;
    let fixture = start(vec![stopped, machine(PENGUIN, "penguin", 2)], cx);
    let menu = fixture.host.menu();
    let actions: Vec<(String, Command)> = menu
        .flat()
        .into_iter()
        .filter_map(|row| match row {
            FlatRow::Action { title, command } => Some((title, command)),
            _ => None,
        })
        .collect();
    assert_eq!(
        actions[0],
        (
            "Start SpaceTerm Core on This Computer".to_owned(),
            Command::Update(crate::updates::presentation::Command::Machine(
                LOCAL,
                MachineAction::Start
            ))
        )
    );
    assert!(
        actions
            .iter()
            .any(|(title, _)| title == "Restart SpaceTerm Core on penguin\u{2026}"),
        "a running Machine keeps its restart action: {actions:?}"
    );
}

/// This computer's SpaceTerm Core is off in Login Items, so the service refuses the menu's Start.
/// The menu opens Login Items, where the person turns it on, instead of an alert with only OK.
/// Once the person turns it on there, the menu runs that Start once, without being asked again.
#[gpui::test]
fn a_start_refused_by_login_items_opens_that_list_and_runs_once_turned_on(cx: &mut TestAppContext) {
    let mut stopped = machine(LOCAL, "This Computer", 0);
    stopped.link = MachineLink::Stopped;
    let fixture = start(vec![stopped], cx);
    let off = |off: bool| {
        fixture
            .host
            .login_item_off
            .store(off, std::sync::atomic::Ordering::SeqCst);
    };
    let wait = |cx: &mut TestAppContext| {
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(10));
        cx.run_until_parked();
    };
    off(true);
    let start_command = fixture
        .host
        .menu()
        .flat()
        .into_iter()
        .find_map(|row| match row {
            FlatRow::Action { command, .. }
                if command
                    == Command::Update(crate::updates::presentation::Command::Machine(
                        LOCAL,
                        MachineAction::Start,
                    )) =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("the menu offers Start");
    cx.update(|cx| fixture.menu.perform(start_command, cx));
    cx.run_until_parked();
    fixture.machines.emit(MachineUpdatesEvent::Finished {
        machine: LOCAL,
        action: MachineAction::Start,
        outcome: Err(ActionFailure::LoginItemOff),
    });
    cx.run_until_parked();

    assert_eq!(fixture.host.login_items.get(), 1);
    assert!(fixture.host.alerts.borrow().is_empty());
    let start = (LOCAL, MachineAction::Start);
    assert_eq!(fixture.machines.performed(), std::slice::from_ref(&start));

    // The item stays off for a while, so nothing starts.
    wait(cx);
    assert_eq!(fixture.machines.performed(), std::slice::from_ref(&start));

    // The person turns SpaceTerm Core on in System Settings.
    off(false);
    wait(cx);
    assert_eq!(fixture.machines.performed(), [start.clone(), start.clone()]);
    fixture.machines.emit(MachineUpdatesEvent::Finished {
        machine: LOCAL,
        action: MachineAction::Start,
        outcome: Ok(()),
    });
    wait(cx);
    assert_eq!(fixture.machines.performed(), [start.clone(), start]);
    assert_eq!(fixture.host.login_items.get(), 1);
}

#[gpui::test]
fn the_flat_menu_keeps_local_restart_and_stop_and_never_offers_remote_stop(
    cx: &mut TestAppContext,
) {
    let fixture = start(
        vec![
            machine(LOCAL, "This Computer", 2),
            machine(PENGUIN, "penguin", 0),
        ],
        cx,
    );
    let titles = |menu: Menu| {
        menu.flat()
            .into_iter()
            .filter_map(|row| match row {
                FlatRow::Action {
                    title,
                    command: Command::Update(_),
                } => Some(title),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        titles(fixture.host.menu()),
        [
            "Restart SpaceTerm Core on This Computer\u{2026}",
            "Stop SpaceTerm Core on This Computer\u{2026}",
            "Restart SpaceTerm Core on penguin",
        ],
    );

    changed(&fixture, waiting(machine(LOCAL, "This Computer", 2)), cx);
    assert_eq!(
        titles(fixture.host.menu()),
        [
            "Restart to Update on This Computer\u{2026}",
            "Stop SpaceTerm Core on This Computer\u{2026}",
            "Restart SpaceTerm Core on penguin",
        ],
    );

    let mut stopping = machine(LOCAL, "This Computer", 2);
    stopping.facts.as_mut().unwrap().operation =
        Some(spaceterm_core::update::facts::Operation::Stopping);
    changed(&fixture, stopping, cx);
    assert_eq!(
        titles(fixture.host.menu()),
        ["Restart SpaceTerm Core on penguin"],
    );
}

/// `machine` with a Core that reports no update facts.
fn unreported(id: MachineId, name: &'static str) -> MachineUpdate {
    let mut update = machine(id, name, 0);
    update.link = MachineLink::Unreported;
    update.facts = None;
    update
}

/// A Core that reports no facts is asked to preview first, and the person confirms the
/// sessions it reports. Nothing runs before the Core answers, and nothing runs through the
/// unpreviewed path.
#[gpui::test]
fn a_machine_without_facts_restarts_and_stops_only_on_its_cores_preview(cx: &mut TestAppContext) {
    let fixture = start(vec![unreported(LOCAL, "This Mac")], cx);
    fixture.machines.hold_previews();
    for (title, action, button) in [
        ("Restart SpaceTerm Core", MachineAction::Restart, "Restart"),
        ("Stop SpaceTerm Core", MachineAction::Stop, "Stop"),
    ] {
        let executed = fixture.machines.executed().len();
        let command = submenu_command(&fixture.host.menu(), LOCAL, title);
        cx.update(|cx| fixture.menu.perform(command, cx));
        cx.run_until_parked();
        assert_eq!(
            fixture.machines.asked().last(),
            Some(&(LOCAL, action.clone()))
        );
        assert_eq!(
            fixture.machines.executed().len(),
            executed,
            "nothing runs before the Core answers"
        );

        fixture.machines.answer_preview(|machine, action| {
            Ok(crate::updates::machines::testing::preview(
                machine, action, 3,
            ))
        });
        cx.run_until_parked();
        let alert = fixture
            .host
            .alerts
            .borrow()
            .last()
            .cloned()
            .expect("it asks");
        assert_eq!(alert.confirm.as_deref(), Some(button));
        assert!(
            alert
                .message
                .starts_with("This will close your 3 running sessions."),
            "the alert names the sessions the Core reports: {}",
            alert.message
        );
        assert_eq!(
            fixture.machines.executed().len(),
            executed,
            "nothing runs while it asks"
        );
        fixture.host.answer(true);
        cx.run_until_parked();
        assert_eq!(
            fixture.machines.executed_actions().last(),
            Some(&(LOCAL, action))
        );
    }
    assert_eq!(fixture.machines.executed_actions().len(), 2);
    assert!(
        fixture.machines.performed().is_empty(),
        "a Restart or Stop never runs without its preview"
    );
}

/// When the Core refuses a preview it issued, the menu previews again and asks, even with no
/// session open. It never runs the action again by itself.
#[gpui::test]
fn a_refused_preview_never_runs_again_without_a_fresh_confirmation(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)],
        "no session closes, so it restarts at once"
    );

    for approved in [true, false] {
        fixture.machines.emit(MachineUpdatesEvent::Finished {
            machine: PENGUIN,
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Changed),
        });
        cx.run_until_parked();
        let executed = fixture.machines.executed().len();
        let alert = fixture
            .host
            .alerts
            .borrow()
            .last()
            .cloned()
            .expect("it asks again");
        assert_eq!(alert.confirm.as_deref(), Some("Restart"));
        fixture.host.answer(approved);
        cx.run_until_parked();
        assert_eq!(
            fixture.machines.executed().len(),
            executed + usize::from(approved),
            "only an approval runs it"
        );
    }
    assert_eq!(
        fixture.machines.asked().len(),
        3,
        "each round previews once"
    );
    assert!(fixture.machines.performed().is_empty());
}

/// The menu showed no sessions, so its restart asks nothing, but the Core's preview finds two:
/// the menu asks with the Core's count before anything runs.
#[gpui::test]
fn sessions_the_core_previews_ask_even_when_the_menu_showed_none(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    fixture.machines.hold_previews();
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    fixture.machines.answer_preview(|machine, action| {
        Ok(crate::updates::machines::testing::preview(
            machine, action, 2,
        ))
    });
    cx.run_until_parked();
    assert!(fixture.machines.executed().is_empty());
    assert_eq!(
        fixture
            .host
            .alerts
            .borrow()
            .last()
            .map(|alert| alert.message.clone()),
        Some("This will close your 2 running sessions.".to_owned())
    );
    fixture.host.answer(false);
    cx.run_until_parked();
    assert!(
        fixture.machines.executed().is_empty(),
        "Cancel runs nothing"
    );
    assert_eq!(fixture.machines.asked().len(), 1);
}

/// One Machine's preview failing in Restart All reports that Machine alone, at once. The others
/// still ask, with their own sessions, and run.
#[gpui::test]
fn a_failed_preview_in_restart_all_leaves_the_other_machines_to_restart(cx: &mut TestAppContext) {
    use crate::updates::machines::testing::preview;

    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Mac", 3)),
            waiting(machine(PENGUIN, "penguin", 4)),
            waiting(machine(BUILD_BOX, "build-box", 1)),
        ],
        cx,
    );
    fixture.machines.hold_previews();
    let restart_all = action_command(&fixture.host.menu(), "Restart All\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart_all, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.asked().len(),
        3,
        "every Core previews at once"
    );

    // build-box answers first, with a failure, while the others still preview.
    fixture.host.answers.borrow_mut().push(true);
    fixture
        .machines
        .answer_preview_of(BUILD_BOX, |_| Err(ActionFailure::Unreachable));
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().as_slice(),
        [Alert {
            title: "Couldn't restart SpaceTerm Core on build-box".to_owned(),
            message: "SpaceTerm can't reach build-box. Try again when it is connected.".to_owned(),
            confirm: None,
        }],
        "the failure reports at once, for build-box alone"
    );

    for (id, sessions) in [(LOCAL, 3), (PENGUIN, 4)] {
        fixture
            .machines
            .answer_preview_of(id, |action| Ok(preview(id, action, sessions)));
    }
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().last().cloned(),
        Some(Alert {
            title: "Restart SpaceTerm Core on 2 machines?".to_owned(),
            message: "This will close 7 running sessions.".to_owned(),
            confirm: Some("Restart".to_owned()),
        })
    );
    assert!(fixture.machines.executed().is_empty());
    fixture.host.answer(true);
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [
            (LOCAL, MachineAction::Restart),
            (PENGUIN, MachineAction::Restart)
        ]
    );
}

/// The Core updated by itself while its alert was open, because its sessions closed. Approving
/// the stale alert restarts nothing more: the update it asked for already ran.
#[gpui::test]
fn an_update_that_ran_while_the_alert_was_open_restarts_nothing_more(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update\u{2026}");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert_eq!(fixture.host.alerts.borrow().len(), 1);

    let mut updated = machine(PENGUIN, "penguin", 0);
    updated.facts.as_mut().unwrap().running = Version::parse("0.9.0").unwrap();
    changed(&fixture, updated, cx);
    fixture.host.answer(true);
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.asked().len(),
        2,
        "the stale approval previews again"
    );
    assert!(
        fixture.machines.executed().is_empty(),
        "a plain restart nobody asked for never runs"
    );
    assert_eq!(fixture.host.alerts.borrow().len(), 1, "and nothing asks");
}
