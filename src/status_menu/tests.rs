use std::cell::RefCell;
use std::rc::Rc;

use gpui::{App, TestAppContext};
use spaceterm_core::protocol::capability::{Capabilities, Capability};
use spaceterm_core::update::facts::{
    PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts, Version,
};

use super::model::{Command, FlatRow, MachineRow, Menu};
use super::{Alert, StatusHost, StatusMenu};
use crate::updates::copy::settings as copy;
use crate::updates::machines::testing::ScriptedMachines;
use crate::updates::machines::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::state;

const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
const BUILD_BOX: MachineId = [3; 32];

fn machine(id: MachineId, name: &'static str, sessions: u32) -> MachineUpdate {
    MachineUpdate {
        detailed: None,
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
        bundled: None,
        failed_push: None,
        asked: None,
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
}

impl StatusHost for Host {
    fn render(&self, menu: &Menu) {
        self.menus.borrow_mut().push(menu.clone());
    }

    fn alert(&self, alert: Alert, _: Option<String>, answered: Answered, _: &mut App) {
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
    row(menu, machine)
        .items
        .iter()
        .find(|item| item.title.as_ref() == title)
        .map(|item| item.command.clone())
        .unwrap_or_else(|| panic!("{title} is offered"))
}

fn row(menu: &Menu, machine: MachineId) -> &MachineRow {
    menu.machines
        .iter()
        .find(|row| row.machine == machine)
        .expect("the Machine has a row")
}

/// The first item of `machine`'s submenu: its row's action.
fn row_action(menu: &Menu, machine: MachineId) -> Command {
    row(menu, machine)
        .items
        .first()
        .map(|item| item.command.clone())
        .expect("the row has an action")
}

/// Every flat row as text: actions in brackets, separators as "-".
fn flat_text(menu: &Menu) -> Vec<String> {
    menu.flat()
        .iter()
        .map(|row| match row {
            FlatRow::Text(text) => text.clone(),
            FlatRow::Separator => "-".to_owned(),
            FlatRow::Action { title, .. } => format!("[{title}]"),
        })
        .collect()
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
        row(&menu, machine)
            .items
            .iter()
            .filter(|item| item.title.as_ref() == copy::STOP_CORE)
            .count()
    };
    assert_eq!((stops(LOCAL), stops(PENGUIN)), (1, 0));

    let stop = submenu_command(&menu, LOCAL, copy::STOP_CORE);
    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(stop, None, cx));
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
fn a_restart_drawn_before_more_sessions_opened_asks_with_the_new_count(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 1))], cx);
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    // Someone opened another session on penguin after the menu drew its restart.
    changed(&fixture, waiting(machine(PENGUIN, "penguin", 2)), cx);

    cx.update(|cx| fixture.menu.perform(restart, None, cx));
    cx.run_until_parked();
    assert!(fixture.machines.executed_actions().is_empty());
    let message = fixture
        .host
        .alerts
        .borrow()
        .last()
        .map(|alert| alert.message.clone())
        .expect("the restart asks first");
    assert!(message.contains("2 Terminal Sessions"), "{message}");
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
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
    cx.run_until_parked();
    changed(&fixture, waiting(machine(PENGUIN, "penguin", 4)), cx);

    fixture.host.answer(true);
    cx.run_until_parked();
    assert!(
        fixture.machines.executed_actions().is_empty(),
        "the approval covered 3 sessions, not 4"
    );
    let message = fixture.host.alerts.borrow().last().unwrap().message.clone();
    assert!(message.contains("4 Terminal Sessions"), "{message}");

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
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
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
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
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
            Command::Update(state::Command::Machine(LOCAL, MachineAction::Start))
        )
    );
    assert_eq!(
        actions[1..],
        [
            ("SpaceTerm Settings…".to_owned(), Command::Settings),
            ("Quit SpaceTerm".to_owned(), Command::Quit),
        ],
        "an up-to-date remote Core offers no action"
    );
}

/// This computer's SpaceTerm Core is off in Login Items, so the service refuses the menu's Start.
/// The menu opens Login Items, where the person turns it on, instead of an alert with only OK.
/// The refused Start never runs by itself: the menu keeps offering Start for the person to choose.
#[gpui::test]
fn a_start_refused_by_login_items_opens_that_list_and_never_runs_by_itself(
    cx: &mut TestAppContext,
) {
    let mut stopped = machine(LOCAL, "This Computer", 0);
    stopped.link = MachineLink::Stopped;
    let fixture = start(vec![stopped], cx);
    let start_command = fixture
        .host
        .menu()
        .flat()
        .into_iter()
        .find_map(|row| match row {
            FlatRow::Action { command, .. }
                if command
                    == Command::Update(state::Command::Machine(LOCAL, MachineAction::Start)) =>
            {
                Some(command)
            }
            _ => None,
        })
        .expect("the menu offers Start");
    cx.update(|cx| fixture.menu.perform(start_command.clone(), None, cx));
    cx.run_until_parked();
    fixture.machines.emit(MachineUpdatesEvent::Finished {
        machine: LOCAL,
        action: MachineAction::Start,
        outcome: Err(ActionFailure::LoginItemOff),
    });
    cx.run_until_parked();

    assert_eq!(fixture.host.login_items.get(), 1);
    assert!(fixture.host.alerts.borrow().is_empty());

    cx.executor()
        .advance_clock(std::time::Duration::from_secs(60 * 60));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.performed(),
        [(LOCAL, MachineAction::Start)],
        "nothing starts by itself"
    );
    assert!(fixture.host.menu().offers(&start_command));
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
fn a_machine_without_facts_stops_only_on_its_cores_preview(cx: &mut TestAppContext) {
    let fixture = start(vec![unreported(LOCAL, "This Mac")], cx);
    fixture.machines.hold_previews();
    assert!(
        fixture.host.menu().offers(&submenu_command(
            &fixture.host.menu(),
            LOCAL,
            copy::STOP_CORE
        )),
        "Stop is offered without facts"
    );
    let executed = fixture.machines.executed().len();
    let command = submenu_command(&fixture.host.menu(), LOCAL, copy::STOP_CORE);
    cx.update(|cx| fixture.menu.perform(command, None, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.asked().last(),
        Some(&(LOCAL, MachineAction::Stop))
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
    assert_eq!(alert.confirm.as_deref(), Some("Stop"));
    assert!(
        alert.message.starts_with("This ends 3 Terminal Sessions."),
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
        Some(&(LOCAL, MachineAction::Stop))
    );
    assert_eq!(fixture.machines.executed_actions().len(), 1);
    assert!(
        fixture.machines.performed().is_empty(),
        "Stop never runs without its preview"
    );
}

/// When the Core refuses a preview it issued, the menu previews again and asks, even with no
/// session open. It never runs the action again by itself.
#[gpui::test]
fn a_refused_preview_never_runs_again_without_a_fresh_confirmation(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 2))], cx);
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)],
        "the person approved it"
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
        assert!(alert.confirm.is_some(), "it asks to confirm");
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

/// The menu showed one session, but the Core's preview finds two: the alert names the Core's
/// count before anything runs.
#[gpui::test]
fn the_alert_names_the_sessions_the_core_previews(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 1))], cx);
    fixture.machines.hold_previews();
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
    cx.run_until_parked();
    fixture.machines.answer_preview(|machine, action| {
        Ok(crate::updates::machines::MachinePreview {
            target: Some(Version::parse("0.9.0").unwrap()),
            ..crate::updates::machines::testing::preview(machine, action, 2)
        })
    });
    cx.run_until_parked();
    assert!(fixture.machines.executed().is_empty());
    assert_eq!(
        fixture
            .host
            .alerts
            .borrow()
            .last()
            .map(|alert| alert.message.contains("2 Terminal Sessions")),
        Some(true),
        "the alert names the 2 Terminal Sessions the Core previews"
    );
    fixture.host.answer(false);
    cx.run_until_parked();
    assert!(
        fixture.machines.executed().is_empty(),
        "Cancel runs nothing"
    );
    assert_eq!(fixture.machines.asked().len(), 1);
}

/// The Core updated by itself while its alert was open, because its sessions closed. Approving
/// the stale alert restarts nothing more: the update it asked for already ran.
#[gpui::test]
fn an_update_that_ran_while_the_alert_was_open_restarts_nothing_more(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 3))], cx);
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
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

/// I7: the menu bar extra shows each Machine with the update view's own row: its name, its
/// version and status on one line, and its action first in the submenu, with the same command.
#[gpui::test]
fn each_machine_row_says_what_the_update_view_says(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            machine(PENGUIN, "penguin", 0),
            waiting(machine(LOCAL, "This Mac", 2)),
            waiting(machine(BUILD_BOX, "build-box", 0)),
        ],
        cx,
    );
    let view = cx.update(|cx| state::current(cx));
    let menu = fixture.host.menu();
    assert_eq!(
        menu.machines
            .iter()
            .map(|row| row.machine)
            .collect::<Vec<_>>(),
        [LOCAL, PENGUIN, BUILD_BOX],
        "local first, then store order"
    );
    for shown in &menu.machines {
        let expected = view
            .machine_row(shown.machine)
            .expect("the view has the row");
        assert_eq!(shown.name, expected.name);
        assert_eq!(shown.status, expected.status);
        assert_eq!(shown.accessibility, expected.accessibility);
        if let Some(status) = expected.status.as_deref()
            && expected.status != expected.version
        {
            assert_eq!(
                shown.line,
                copy::line(expected.version.as_deref(), Some(status))
            );
        }
        let first = shown.items.first();
        match expected.action {
            Some(action) => {
                let first = first.expect("the row's action leads the submenu");
                assert_eq!(first.title, action.label);
                assert_eq!(first.subtitle, action.subtitle);
                assert_eq!(first.command, Command::Update(action.command));
            }
            None => assert!(
                first.is_none_or(|item| item.title.as_ref() == copy::STOP_CORE),
                "a row without an action offers only Stop"
            ),
        }
    }
    let app = view.app_row();
    assert_eq!(
        menu.header,
        copy::line(app.version.as_deref(), app.status.as_deref())
    );
}

/// A settled Machine shows its version and its Terminal Sessions, with no zero count.
#[gpui::test]
fn a_settled_machine_shows_its_version_and_terminal_sessions(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            machine(LOCAL, "This Mac", 0),
            machine(PENGUIN, "penguin", 3),
        ],
        cx,
    );
    let menu = fixture.host.menu();
    let view = cx.update(|cx| state::current(cx));
    for (id, sessions) in [
        (LOCAL, "No Terminal Sessions"),
        (PENGUIN, "3 Terminal Sessions"),
    ] {
        let expected = view.machine_row(id).unwrap();
        assert_eq!(
            row(&menu, id).line,
            format!("0.8.0 \u{b7} {sessions}"),
            "{expected:?}"
        );
    }
    assert_eq!(
        row(&menu, LOCAL).items[0].subtitle,
        None,
        "Stop names no zero count"
    );
}

/// The menu is SpaceTerm, then the SpaceTerm Core section, then Settings and Quit. Recent
/// Updates, Update All, What's New and the old relaunch and retry items are gone.
#[gpui::test]
fn the_menu_is_spaceterm_then_its_cores_then_settings_and_quit(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Computer", 2)),
            waiting(machine(PENGUIN, "penguin", 3)),
        ],
        cx,
    );
    let menu = fixture.host.menu();
    let rows = flat_text(&menu);
    assert_eq!(rows[0], format!("SpaceTerm \u{b7} {}", menu.header));
    let heading = rows
        .iter()
        .position(|row| row == "SpaceTerm Core")
        .expect("the section has its heading");
    assert!(
        rows[heading + 1].starts_with("This Computer \u{b7} "),
        "{rows:?}"
    );
    assert!(rows[heading + 2].starts_with("penguin \u{b7} "), "{rows:?}");
    assert_eq!(
        rows[rows.len() - 3..],
        ["-", "[SpaceTerm Settings\u{2026}]", "[Quit SpaceTerm]"]
    );
    let commands: Vec<Command> = menu.commands().collect();
    assert_eq!(
        commands[commands.len() - 2..],
        [Command::Settings, Command::Quit]
    );
    for removed in [
        "Recent",
        "Update All",
        "What's New",
        "Relaunch to Update SpaceTerm",
        "Retry",
        "Open",
        "Machines",
        "Update This",
        "Update penguin",
    ] {
        assert!(
            !rows.iter().any(|row| row.contains(removed)),
            "{removed}: {rows:?}"
        );
    }
}

/// A flat menu has no submenu to name the Machine, so a Core restart names its subject.
#[gpui::test]
fn the_flat_menu_names_the_subject_of_each_machine_action(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Computer", 2)),
            waiting(machine(PENGUIN, "penguin", 3)),
        ],
        cx,
    );
    let menu = fixture.host.menu();
    let actions: Vec<(String, Command)> = menu
        .flat()
        .into_iter()
        .filter_map(|row| match row {
            FlatRow::Action { title, command } => Some((title, command)),
            _ => None,
        })
        .collect();
    for (id, name) in [(LOCAL, "This Computer"), (PENGUIN, "penguin")] {
        let restart = row_action(&menu, id);
        let (title, _) = actions
            .iter()
            .find(|(_, command)| *command == restart)
            .expect("the restart is in the flat menu");
        assert_eq!(
            title.trim_end_matches('\u{2026}'),
            format!("Restart SpaceTerm Core on {name}")
        );
    }
    assert!(
        actions
            .iter()
            .any(|(title, _)| title == "Stop SpaceTerm Core on This Computer\u{2026}"),
        "{actions:?}"
    );
    for (_, command) in &actions {
        assert!(menu.offers(command), "{command:?}");
    }
}

/// A downloaded SpaceTerm update adds Relaunch to Update, which keeps Terminal Sessions running
/// and relaunches without asking.
#[gpui::test]
fn relaunch_to_update_relaunches_without_asking(cx: &mut TestAppContext) {
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
    let step = menu.step.clone().expect("SpaceTerm has a step");
    assert_eq!(step.title.as_ref(), copy::RELAUNCH_TO_UPDATE);
    assert_eq!(step.subtitle.as_deref(), Some(copy::KEEPS_RUNNING));
    let view = cx.update(|cx| state::current(cx));
    assert_eq!(
        Some(step.command.clone()),
        view.app_row()
            .action
            .map(|action| Command::Update(action.command)),
        "the step is the SpaceTerm row's action"
    );

    cx.update(|cx| fixture.menu.perform(step.command, None, cx));
    cx.run_until_parked();
    assert!(fixture.host.alerts.borrow().is_empty());
    assert_eq!(updater.installations.get(), 1);
    assert_eq!(updater.downloads.get(), 1, "nothing downloads again");
}

/// From the Insist stage the status item's symbol shows a dot; before it, none.
#[gpui::test]
fn the_symbol_shows_a_dot_from_the_insist_stage(cx: &mut TestAppContext) {
    let fresh = waiting(machine(LOCAL, "This Mac", 2));
    let fixture = start(vec![fresh.clone()], cx);
    assert!(!fixture.host.menu().attention, "Offer shows no dot");

    let mut waited = fresh;
    waited
        .facts
        .as_mut()
        .unwrap()
        .pending
        .as_mut()
        .unwrap()
        .installed_at = crate::updates::now() - crate::updates::ladder::INSIST_AFTER - 60;
    changed(&fixture, waited, cx);
    let menu = fixture.host.menu();
    assert!(menu.attention, "Insist shows the dot");
    assert_eq!(menu.attention, cx.update(|cx| state::current(cx).insists()));
}

/// With automatic downloads off, an available SpaceTerm update adds Download Update, which
/// downloads and never relaunches, even once the download is ready.
#[gpui::test]
fn download_update_downloads_and_never_relaunches(cx: &mut TestAppContext) {
    use crate::updates::policy::UpdatePreferences;
    use crate::updates::testing::RecordingAdapter;
    use crate::updates::{ApplicationUpdates, UpdateEvent, UpdateService};
    let updater = Rc::new(RecordingAdapter::available());
    cx.update(|cx| ApplicationUpdates::install(updater.clone(), cx));
    let service = cx.update(|cx| cx.global::<UpdateService>().0.clone());
    let document = crate::settings::SettingsDocument {
        updates: UpdatePreferences {
            automatic_downloads: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let settings = crate::settings::Settings::load(
        crate::settings::storage::testing::MemoryStorage::with_document(&document),
    );
    service.update(cx, |updates, _| updates.attach_settings(settings));
    service.update(cx, |updates, cx| updates.check(false, cx));
    updater.emit(UpdateEvent::Available("0.9.0".into()));
    cx.run_until_parked();
    let fixture = start(vec![machine(PENGUIN, "penguin", 3)], cx);
    let step = fixture.host.menu().step.expect("SpaceTerm has a step");
    assert_eq!(step.title.as_ref(), "Download Update");
    assert_eq!(updater.downloads.get(), 0);

    cx.update(|cx| fixture.menu.perform(step.command, None, cx));
    cx.run_until_parked();
    assert_eq!(updater.downloads.get(), 1, "it downloads");
    updater.emit(UpdateEvent::Ready);
    cx.run_until_parked();
    assert_eq!(updater.installations.get(), 0, "it never relaunches");
    assert!(fixture.host.alerts.borrow().is_empty());
    let step = fixture
        .host
        .menu()
        .step
        .expect("the relaunch stays a choice");
    assert_eq!(step.title.as_ref(), "Relaunch to Update");
}

/// I7 for the menu bar extra: choosing a waiting Machine's first item asks that Machine's Core
/// for the restart its row names, as Settings and the card do.
#[gpui::test]
fn choosing_a_rows_action_asks_that_machines_core(cx: &mut TestAppContext) {
    let fixture = start(
        vec![
            waiting(machine(LOCAL, "This Mac", 2)),
            waiting(machine(PENGUIN, "penguin", 3)),
        ],
        cx,
    );
    let menu = fixture.host.menu();
    let first = &row(&menu, PENGUIN).items[0];
    assert_eq!(first.title.as_ref(), "Restart Core\u{2026}");
    assert_eq!(first.subtitle.as_deref(), Some("Ends 3 Terminal Sessions"));
    cx.update(|cx| fixture.menu.perform(first.command.clone(), None, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.asked(),
        [(PENGUIN, MachineAction::Restart)]
    );
    assert!(fixture.machines.executed().is_empty(), "it asks first");
}

/// An approved restart whose lane drops while its result is followed reports through the title
/// bar's step, not through an alert the person did not ask for (design 4.6).
#[gpui::test]
fn a_restart_that_fails_after_it_ran_opens_no_alert(cx: &mut TestAppContext) {
    let fixture = start(vec![waiting(machine(PENGUIN, "penguin", 2))], cx);
    let restart = row_action(&fixture.host.menu(), PENGUIN);
    fixture.host.answers.borrow_mut().push(true);
    cx.update(|cx| fixture.menu.perform(restart, None, cx));
    cx.run_until_parked();
    assert_eq!(
        fixture.machines.executed_actions(),
        [(PENGUIN, MachineAction::Restart)]
    );
    fixture.machines.emit(MachineUpdatesEvent::Finished {
        machine: PENGUIN,
        action: MachineAction::Restart,
        outcome: Err(ActionFailure::Unreachable),
    });
    cx.run_until_parked();
    assert_eq!(
        fixture.host.alerts.borrow().len(),
        1,
        "only the confirmation opened"
    );
}
