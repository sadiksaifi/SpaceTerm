use std::collections::VecDeque;
use std::sync::mpsc;

use spaceterm_core::update::facts::{PendingVersion, Platform, Protocols, UpdateCheck};

use super::*;
use crate::updates::presentation::{CheckProblem, Dismissed, MachineState, MachineView};

const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
/// Long enough for a follower thread to answer; a missing event fails within it.
const PROMPT: Duration = Duration::from_secs(5);

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn facts(revision: u64, sessions: u32, pending: Option<&str>) -> Observed {
    Observed {
        boot: "boot-1".into(),
        facts: UpdateFacts {
            revision,
            platform: Platform::LinuxX86_64,
            running: version("0.8.0"),
            pending: pending.map(|to| PendingVersion {
                version: version(to),
                protocols: Protocols::CURRENT,
                breaking: false,
                installed_at: 1,
                overdue_since: None,
                stranded: Vec::new(),
            }),
            sessions,
            operation: None,
            check: UpdateCheck::Never,
            skipped: None,
            last_failure: None,
        },
    }
}

/// A service lane the test scripts: `Facts` answers from a queue, `WaitFacts` blocks until the
/// test sends its answer, and every request is recorded.
struct ScriptedLane {
    facts: Mutex<VecDeque<Result<Observed, ActionFailure>>>,
    waits: Mutex<mpsc::Receiver<Result<Observed, ActionFailure>>>,
    answer: Mutex<mpsc::Sender<Result<Observed, ActionFailure>>>,
    requests: Mutex<Vec<String>>,
}

impl ScriptedLane {
    fn new(facts: impl IntoIterator<Item = Result<Observed, ActionFailure>>) -> Arc<Self> {
        let (answer, waits) = mpsc::channel();
        Arc::new(Self {
            facts: Mutex::new(facts.into_iter().collect()),
            waits: Mutex::new(waits),
            answer: Mutex::new(answer),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Answers the pending `WaitFacts`.
    fn answer(&self, reply: Result<Observed, ActionFailure>) {
        self.answer.lock().unwrap().send(reply).unwrap();
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn record(&self, request: String) {
        self.requests.lock().unwrap().push(request);
    }
}

impl MachineLane for ScriptedLane {
    fn facts(&self) -> Result<Observed, ActionFailure> {
        self.record("facts".into());
        self.facts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Err(ActionFailure::Unreachable))
    }

    fn wait_facts(&self, boot: &str, revision: u64) -> Result<Observed, ActionFailure> {
        self.record(format!("wait {boot} {revision}"));
        self.waits
            .lock()
            .unwrap()
            .recv_timeout(PROMPT)
            .unwrap_or(Err(ActionFailure::Unreachable))
    }

    fn check(&self) -> Result<(), ActionFailure> {
        self.record("check".into());
        Ok(())
    }

    fn install(&self) -> Result<(), ActionFailure> {
        self.record("install".into());
        Ok(())
    }

    fn change(&self, change: LaneChange, sessions: Option<u32>) -> Result<(), ActionFailure> {
        self.record(format!("{change:?} {sessions:?}"));
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Script {
    Stopped,
    Offline,
    Lane,
}

struct ScriptedMachine {
    script: Mutex<Script>,
    lane: Arc<ScriptedLane>,
    starts: Mutex<u32>,
}

impl ScriptedMachine {
    fn new(script: Script, lane: Arc<ScriptedLane>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            lane,
            starts: Mutex::new(0),
        })
    }

    fn set(&self, script: Script) {
        *self.script.lock().unwrap() = script;
    }
}

impl Machine for ScriptedMachine {
    fn reach(&self) -> Reach {
        match *self.script.lock().unwrap() {
            Script::Stopped => Reach::Stopped,
            Script::Offline => Reach::Offline,
            Script::Lane => Reach::Lane {
                lane: Arc::clone(&self.lane) as Arc<dyn MachineLane>,
                work: Ok(()),
            },
        }
    }

    fn start(&self) -> Result<(), ActionFailure> {
        *self.starts.lock().unwrap() += 1;
        Ok(())
    }
}

#[derive(Default)]
struct ScriptedSources {
    known: Mutex<Vec<Known>>,
    changed: Mutex<Option<Changed>>,
}

impl ScriptedSources {
    fn with(known: Vec<Known>) -> Arc<Self> {
        Arc::new(Self {
            known: Mutex::new(known),
            changed: Mutex::default(),
        })
    }

    /// Reports a change, as the service watch or the route registry does.
    fn change(&self) {
        let changed = self.changed.lock().unwrap().clone().expect("watching");
        changed();
    }
}

impl Sources for ScriptedSources {
    fn known(&self) -> Vec<Known> {
        self.known.lock().unwrap().clone()
    }

    fn watch(&self, changed: Changed) -> Result<(), MachineUpdatesError> {
        *self.changed.lock().unwrap() = Some(changed);
        Ok(())
    }
}

fn known(id: MachineId, name: &str, local: bool, machine: &Arc<ScriptedMachine>) -> Known {
    Known {
        id,
        name: SharedString::from(name.to_owned()),
        local,
        machine: Arc::clone(machine) as Arc<dyn Machine>,
    }
}

fn started(
    sources: &Arc<ScriptedSources>,
    restart_limit: Duration,
) -> (LiveMachines, async_channel::Receiver<MachineUpdatesEvent>) {
    let adapter =
        LiveMachines::with_restart_limit(Arc::clone(sources) as Arc<dyn Sources>, restart_limit);
    let (events, received) = async_channel::unbounded();
    adapter.start(events).unwrap();
    (adapter, received)
}

/// The first event `matches` accepts, skipping the others.
fn until<T>(
    events: &async_channel::Receiver<MachineUpdatesEvent>,
    what: &str,
    mut matches: impl FnMut(&MachineUpdatesEvent) -> Option<T>,
) -> T {
    let deadline = Instant::now() + PROMPT;
    loop {
        match events.try_recv() {
            Ok(event) => {
                if let Some(found) = matches(&event) {
                    return found;
                }
            }
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => panic!("{what} is reported"),
        }
    }
}

fn changed(
    events: &async_channel::Receiver<MachineUpdatesEvent>,
    what: &str,
    accept: impl Fn(&MachineUpdate) -> bool,
) -> MachineUpdate {
    until(events, what, |event| match event {
        MachineUpdatesEvent::Changed(update) if accept(update) => Some((**update).clone()),
        _ => None,
    })
}

fn finished(
    events: &async_channel::Receiver<MachineUpdatesEvent>,
    expected: &MachineAction,
) -> Result<(), ActionFailure> {
    until(events, "the action finishing", |event| match event {
        MachineUpdatesEvent::Finished {
            action, outcome, ..
        } if action == expected => Some(*outcome),
        _ => None,
    })
}

/// The adapter lists This Mac first, reads its facts, then long-polls `WaitFacts` with the run
/// and revision it holds, and reports each answer.
#[test]
fn a_local_core_reports_its_facts_and_follows_each_change() {
    let lane = ScriptedLane::new([Ok(facts(1, 3, None))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(LOCAL, "This Mac", true, &machine)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);

    let listed = until(&events, "the Machines", |event| match event {
        MachineUpdatesEvent::Machines(machines) => Some(machines.clone()),
        _ => None,
    });
    assert_eq!(listed.len(), 1);
    assert!(listed[0].local);
    let first = changed(&events, "the first facts", |update| update.facts.is_some());
    assert_eq!(first.link, MachineLink::Connected);
    assert_eq!(first.facts.as_ref().unwrap().sessions, 3);

    lane.answer(Ok(facts(2, 3, Some("0.9.0"))));
    let next = changed(&events, "the changed facts", |update| {
        update
            .facts
            .as_ref()
            .is_some_and(|facts| facts.revision == 2)
    });
    assert_eq!(
        next.facts.unwrap().pending.unwrap().version,
        version("0.9.0")
    );
    let requests = lane.requests();
    assert_eq!(requests[0], "facts");
    assert_eq!(requests[1], "wait boot-1 1");
}

/// A Core that answers `Facts` with `unsupported` reads as "Can't check", not as a missing
/// Machine or one that stays connecting.
#[test]
fn a_core_that_predates_update_facts_reads_cant_check() {
    let lane = ScriptedLane::new([Err(ActionFailure::Unavailable(Unavailable::CoreTooOld))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);

    let update = changed(&events, "the unreported Core", |update| {
        update.link == MachineLink::Unreported
    });
    let view = MachineView::new(&update, &Dismissed::new(), crate::updates::now());
    assert_eq!(
        view.state,
        MachineState::CantCheck {
            reason: CheckProblem::Unreported,
            push: None,
        }
    );
}

/// A deliberately stopped local Core reads as Stopped from the service manager alone. Start
/// runs through the service manager, and the started Core's facts follow the next change.
#[test]
fn a_stopped_local_core_reads_stopped_and_starts_through_the_service_manager() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, None))]);
    let machine = ScriptedMachine::new(Script::Stopped, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(LOCAL, "This Mac", true, &machine)]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);

    changed(&events, "the stopped Core", |update| {
        update.link == MachineLink::Stopped
    });
    assert!(lane.requests().is_empty(), "a stopped Core is not dialed");

    adapter.perform(LOCAL, MachineAction::Start);
    assert_eq!(finished(&events, &MachineAction::Start), Ok(()));
    assert_eq!(*machine.starts.lock().unwrap(), 1);

    machine.set(Script::Lane);
    sources.change();
    let running = changed(&events, "the started Core", |update| {
        update.link == MachineLink::Connected && update.facts.is_some()
    });
    assert_eq!(running.facts.unwrap().sessions, 0);
}

/// Install, Check Again, Restart and Stop each become their lane request. Restart carries the
/// session count the person saw, and Stop is refused on a paired Machine without dialing it.
#[test]
fn actions_become_lane_requests_and_stop_stays_local() {
    let remote_lane = ScriptedLane::new([Ok(facts(1, 3, Some("0.9.0")))]);
    let remote = ScriptedMachine::new(Script::Lane, Arc::clone(&remote_lane));
    let local_lane = ScriptedLane::new([Ok(facts(1, 2, None))]);
    let local = ScriptedMachine::new(Script::Lane, Arc::clone(&local_lane));
    let sources = ScriptedSources::with(vec![
        known(LOCAL, "This Mac", true, &local),
        known(PENGUIN, "penguin", false, &remote),
    ]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    for id in [LOCAL, PENGUIN] {
        changed(&events, "the facts", |update| {
            update.machine == id && update.facts.is_some()
        });
    }

    for action in [
        MachineAction::Install,
        MachineAction::CheckAgain,
        MachineAction::Restart,
    ] {
        adapter.perform(PENGUIN, action.clone());
        assert_eq!(finished(&events, &action), Ok(()));
    }
    adapter.perform(PENGUIN, MachineAction::Stop);
    assert_eq!(
        finished(&events, &MachineAction::Stop),
        Err(ActionFailure::Failed)
    );
    adapter.perform(LOCAL, MachineAction::Stop);
    assert_eq!(finished(&events, &MachineAction::Stop), Ok(()));

    let remote_actions: Vec<String> = remote_lane
        .requests()
        .into_iter()
        .filter(|request| request != "facts" && !request.starts_with("wait"))
        .collect();
    assert_eq!(remote_actions, ["install", "check", "Restart Some(3)"]);
    assert!(local_lane.requests().contains(&"Stop Some(2)".to_owned()));
}

/// A restart whose Core does not answer again before the limit reads "Can't confirm" for the
/// Pending Version it restarted into, rather than Offline.
#[test]
fn a_restart_that_does_not_come_back_reads_cant_confirm() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, Some("0.9.0")))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (adapter, events) = started(&sources, Duration::from_millis(300));
    changed(&events, "the facts", |update| update.facts.is_some());

    adapter.perform(PENGUIN, MachineAction::Restart);
    assert_eq!(finished(&events, &MachineAction::Restart), Ok(()));
    // The Core closes its lane to restart and does not come back.
    lane.answer(Err(ActionFailure::Unreachable));
    let unconfirmed = changed(&events, "the unconfirmed restart", |update| {
        matches!(update.link, MachineLink::Unconfirmed { .. })
    });
    assert!(matches!(
        unconfirmed.link,
        MachineLink::Unconfirmed { expected, .. } if expected == version("0.9.0")
    ));
}

/// A Machine paired while the adapter runs, or removed, lists every Machine again.
#[test]
fn added_and_removed_machines_are_listed_again() {
    let local = ScriptedMachine::new(Script::Offline, ScriptedLane::new([]));
    let remote = ScriptedMachine::new(Script::Offline, ScriptedLane::new([]));
    let sources = ScriptedSources::with(vec![known(LOCAL, "This Mac", true, &local)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);
    let listing = |events: &async_channel::Receiver<MachineUpdatesEvent>| {
        until(events, "the Machines", |event| match event {
            MachineUpdatesEvent::Machines(machines) => {
                Some(machines.iter().map(|m| m.machine).collect::<Vec<_>>())
            }
            _ => None,
        })
    };
    assert_eq!(listing(&events), [LOCAL]);

    sources
        .known
        .lock()
        .unwrap()
        .push(known(PENGUIN, "penguin", false, &remote));
    sources.change();
    assert_eq!(listing(&events), [LOCAL, PENGUIN]);
    let offline = changed(&events, "the offline Machine", |update| {
        update.machine == PENGUIN
    });
    assert!(matches!(offline.link, MachineLink::Offline { .. }));

    sources.known.lock().unwrap().truncate(1);
    sources.change();
    assert_eq!(listing(&events), [LOCAL]);
}

#[test]
fn lane_codes_become_typed_failures() {
    assert_eq!(
        failure_of_code("unsupported"),
        ActionFailure::Unavailable(Unavailable::CoreTooOld)
    );
    assert_eq!(failure_of_code("service_busy"), ActionFailure::Busy);
    assert_eq!(failure_of_code("changed"), ActionFailure::Changed);
    assert_eq!(failure_of_code("not_running"), ActionFailure::Unreachable);
    assert_eq!(failure_of_code("local_only"), ActionFailure::Failed);
}
