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

/// A status item that records each render and alert, and answers alerts as told.
#[derive(Default)]
struct Host {
    menus: RefCell<Vec<Menu>>,
    alerts: RefCell<Vec<Alert>>,
    /// Answers for the next alerts, in order. An alert with no answer left stays open.
    answers: RefCell<Vec<bool>>,
}

impl StatusHost for Host {
    fn render(&self, menu: &Menu) {
        self.menus.borrow_mut().push(menu.clone());
    }

    fn alert(&self, alert: Alert, answered: Box<dyn FnOnce(bool) + Send>, _: &mut App) {
        self.alerts.borrow_mut().push(alert);
        let mut answers = self.answers.borrow_mut();
        if !answers.is_empty() {
            answered(answers.remove(0));
        }
    }
}

impl Host {
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
        StatusMenu::install(
            Rc::clone(&host) as Rc<dyn StatusHost>,
            Some(Rc::clone(&adapter) as Rc<dyn MachineUpdatesAdapter>),
            cx,
        )
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
        fixture.machines.performed().is_empty(),
        "nothing restarts while the alert is open"
    );

    fixture.host.answers.borrow_mut().push(false);
    cx.update(|cx| fixture.menu.perform(restart.clone(), cx));
    cx.run_until_parked();
    assert!(
        fixture.machines.performed().is_empty(),
        "Cancel restarts nothing"
    );

    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(restart, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.performed(),
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
        fixture.machines.performed(),
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
        fixture.machines.performed(),
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
    assert_eq!(fixture.machines.performed(), [(LOCAL, MachineAction::Stop)]);
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
    assert!(fixture.machines.performed().is_empty());
    assert!(fixture.host.alerts.borrow().is_empty());
}

#[gpui::test]
fn a_failed_action_from_the_menu_opens_an_alert_and_others_stay_quiet(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 0))], cx);
    let restart = submenu_command(&fixture.host.menu(), PENGUIN, "Restart to Update");
    cx.update(|cx| fixture.menu.perform(restart, cx));
    // The title-bar control's own action fails too; that surface reports it.
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
            title: "SpaceTerm couldn't restart SpaceTerm Core on penguin.".to_owned(),
            message: "SpaceTerm can't reach penguin.".to_owned(),
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
