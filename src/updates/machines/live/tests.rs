use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use spaceterm_core::service::lane::{LaneAction, LaneEffects};
use spaceterm_core::update::facts::{PendingVersion, Platform, Protocols, UpdateCheck};

use super::*;
use crate::updates::presentation::{CheckProblem, Dismissed, MachineState, MachineView};

const LOCAL: MachineId = [1; 32];
const PENGUIN: MachineId = [2; 32];
/// Long enough for a follower thread to answer; a missing event fails within it.
const PROMPT: Duration = Duration::from_secs(5);
/// How long a test waits to show that something did not happen.
const QUIET: Duration = Duration::from_millis(300);

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
/// test answers it or the follower abandons it, `Preview` ends `previewed` sessions, and every
/// request is recorded.
struct ScriptedLane {
    facts: Mutex<VecDeque<Result<Observed, ActionFailure>>>,
    waits: Mutex<mpsc::Receiver<Result<Observed, ActionFailure>>>,
    answer: Mutex<mpsc::Sender<Result<Observed, ActionFailure>>>,
    previewed: Mutex<u32>,
    requests: Mutex<Vec<String>>,
}

impl ScriptedLane {
    fn new(facts: impl IntoIterator<Item = Result<Observed, ActionFailure>>) -> Arc<Self> {
        let (answer, waits) = mpsc::channel();
        Arc::new(Self {
            facts: Mutex::new(facts.into_iter().collect()),
            waits: Mutex::new(waits),
            answer: Mutex::new(answer),
            previewed: Mutex::new(0),
            requests: Mutex::new(Vec::new()),
        })
    }

    /// Answers the pending `WaitFacts`.
    fn answer(&self, reply: Result<Observed, ActionFailure>) {
        self.answer.lock().unwrap().send(reply).unwrap();
    }

    fn preview_ends(&self, sessions: u32) {
        *self.previewed.lock().unwrap() = sessions;
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self, prefix: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.starts_with(prefix))
            .count()
    }

    /// The requests other than reads.
    fn actions(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .filter(|request| request != "facts" && !request.starts_with("wait"))
            .collect()
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

    fn wait_facts(
        &self,
        boot: &str,
        revision: u64,
        stale: &dyn Fn() -> bool,
    ) -> Result<Observed, ActionFailure> {
        self.record(format!("wait {boot} {revision}"));
        let deadline = Instant::now() + PROMPT;
        let waits = self.waits.lock().unwrap();
        while Instant::now() < deadline && !stale() {
            if let Ok(reply) = waits.recv_timeout(Duration::from_millis(10)) {
                return reply;
            }
        }
        Err(ActionFailure::Unreachable)
    }

    fn check(&self) -> Result<(), ActionFailure> {
        self.record("check".into());
        Ok(())
    }

    fn install(&self, build: Option<&EmbeddedCore>) -> Result<(), ActionFailure> {
        self.record(build.map_or_else(
            || "install".into(),
            |build| format!("push {}", build.candidate.version()),
        ));
        Ok(())
    }

    fn preview(&self, change: LaneChange) -> Result<admin::Preview, ActionFailure> {
        self.record(format!("preview {change:?}"));
        let ends = *self.previewed.lock().unwrap();
        Ok(admin::Preview {
            action: match change {
                LaneChange::Restart => LaneAction::Restart,
                LaneChange::Stop => LaneAction::Stop,
            },
            token: format!("token-{change:?}-{ends}"),
            expires_at: String::new(),
            effects: LaneEffects {
                ends_sessions: ends,
                sessions: Vec::new(),
                start_requires_machine: false,
                from: None,
                to: None,
                to_major: None,
            },
        })
    }

    fn execute(&self, preview: admin::Preview) -> Result<(), ActionFailure> {
        self.record(format!("execute {}", preview.token));
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Script {
    Stopped,
    Offline,
    Incompatible,
    Lane,
}

struct ScriptedMachine {
    script: Mutex<Script>,
    lane: Arc<ScriptedLane>,
    starts: Mutex<u32>,
    retries: Mutex<u32>,
}

impl ScriptedMachine {
    fn new(script: Script, lane: Arc<ScriptedLane>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script),
            lane,
            starts: Mutex::new(0),
            retries: Mutex::new(0),
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
            Script::Incompatible => Reach::Lane {
                lane: Arc::clone(&self.lane) as Arc<dyn MachineLane>,
                work: Err(Incompatible::ListenerTooOld),
            },
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

    fn retry(&self) {
        *self.retries.lock().unwrap() += 1;
    }
}

#[derive(Default)]
struct ScriptedSources {
    known: Mutex<Vec<Known>>,
    changed: Mutex<Option<Changed>>,
    /// Set when the adapter drops its watch.
    unwatched: Arc<AtomicBool>,
}

impl ScriptedSources {
    fn with(known: Vec<Known>) -> Arc<Self> {
        Arc::new(Self {
            known: Mutex::new(known),
            ..Self::default()
        })
    }

    fn set(&self, known: Vec<Known>) {
        *self.known.lock().unwrap() = known;
        self.change();
    }

    /// Reports a change, as the service watch or the route registry does.
    fn change(&self) {
        let changed = self.changed.lock().unwrap().clone().expect("watching");
        changed();
    }
}

struct Unwatch(Arc<AtomicBool>);

impl Drop for Unwatch {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

impl Sources for ScriptedSources {
    fn known(&self) -> Vec<Known> {
        self.known.lock().unwrap().clone()
    }

    fn watch(&self, changed: Changed) -> Result<Watching, MachineUpdatesError> {
        *self.changed.lock().unwrap() = Some(changed);
        Ok(Box::new(Unwatch(Arc::clone(&self.unwatched))))
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

type Events = async_channel::Receiver<MachineUpdatesEvent>;

fn started(sources: &Arc<ScriptedSources>, restart_limit: Duration) -> (LiveMachines, Events) {
    let adapter = LiveMachines::with_restart_limit(
        Arc::clone(sources) as Arc<dyn Sources>,
        Arc::new(EmbeddedCores::new(None, "spaceterm-development")),
        restart_limit,
    );
    let (events, received) = async_channel::unbounded();
    adapter.start(events).unwrap();
    (adapter, received)
}

/// Waits until `condition` holds.
fn eventually(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + PROMPT;
    while !condition() {
        assert!(Instant::now() < deadline, "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The first event `matches` accepts, skipping the others.
fn until<T>(
    events: &Events,
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

fn changed(events: &Events, what: &str, accept: impl Fn(&MachineUpdate) -> bool) -> MachineUpdate {
    until(events, what, |event| match event {
        MachineUpdatesEvent::Changed(update) if accept(update) => Some((**update).clone()),
        _ => None,
    })
}

/// Waits until every Machine in `ids` reported facts, in whatever order they arrive.
fn all_reported(events: &Events, ids: &[MachineId]) {
    let mut waiting: Vec<MachineId> = ids.to_vec();
    until(events, "every Machine's facts", |event| {
        if let MachineUpdatesEvent::Changed(update) = event
            && update.facts.is_some()
        {
            waiting.retain(|id| *id != update.machine);
        }
        waiting.is_empty().then_some(())
    });
}

fn finished(events: &Events, expected: &MachineAction) -> Result<(), ActionFailure> {
    until(events, "the action finishing", |event| match event {
        MachineUpdatesEvent::Finished {
            action, outcome, ..
        } if action == expected => Some(*outcome),
        _ => None,
    })
}

/// The adapter's preview of `action` on `machine`.
fn previewed(
    adapter: &LiveMachines,
    machine: MachineId,
    action: MachineAction,
) -> Result<MachinePreview, ActionFailure> {
    adapter.preview(machine, action).recv_blocking().unwrap()
}

fn listing(events: &Events) -> Vec<MachineId> {
    until(events, "the Machines", |event| match event {
        MachineUpdatesEvent::Machines(machines) => {
            Some(machines.iter().map(|m| m.machine).collect())
        }
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

    assert_eq!(listing(&events), [LOCAL]);
    let first = changed(&events, "the first facts", |update| update.facts.is_some());
    assert_eq!(first.link, MachineLink::Connected);
    assert_eq!(first.facts.as_ref().unwrap().sessions, 3);

    eventually("the long poll", || lane.count("wait") == 1);
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
    assert_eq!(lane.requests()[..2], ["facts", "wait boot-1 1"]);
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

/// A Core that does not answer is read again only after a change its sources report, never in
/// a loop.
#[test]
fn an_unreachable_core_is_read_again_only_after_a_change() {
    let lane = ScriptedLane::new([]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);
    changed(&events, "the offline Machine", |update| {
        matches!(update.link, MachineLink::Offline { .. })
    });
    std::thread::sleep(QUIET);
    assert_eq!(lane.count("facts"), 1, "no retry without a change");

    sources.change();
    eventually("a read after the change", || lane.count("facts") == 2);
    std::thread::sleep(QUIET);
    assert_eq!(lane.count("facts"), 2);
    assert_eq!(
        *machine.retries.lock().unwrap(),
        0,
        "routes keep their backoff"
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

/// Install and Check Again become their lane requests. A Restart performed without a preview
/// executes only the Core's preview of the sessions the person saw. Stop is refused on a paired
/// Machine without dialing it.
#[test]
fn actions_become_lane_requests_and_stop_stays_local() {
    let remote_lane = ScriptedLane::new([Ok(facts(1, 3, Some("0.9.0")))]);
    remote_lane.preview_ends(3);
    let remote = ScriptedMachine::new(Script::Lane, Arc::clone(&remote_lane));
    let local_lane = ScriptedLane::new([Ok(facts(1, 2, None))]);
    local_lane.preview_ends(2);
    let local = ScriptedMachine::new(Script::Lane, Arc::clone(&local_lane));
    let sources = ScriptedSources::with(vec![
        known(LOCAL, "This Mac", true, &local),
        known(PENGUIN, "penguin", false, &remote),
    ]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    all_reported(&events, &[LOCAL, PENGUIN]);

    for action in [MachineAction::Install, MachineAction::CheckAgain] {
        adapter.perform(PENGUIN, action.clone());
        assert_eq!(finished(&events, &action), Ok(()));
    }
    adapter.execute(previewed(&adapter, PENGUIN, MachineAction::Restart).unwrap());
    assert_eq!(finished(&events, &MachineAction::Restart), Ok(()));
    assert_eq!(
        previewed(&adapter, PENGUIN, MachineAction::Stop),
        Err(ActionFailure::Failed),
        "Stop is This Mac's alone"
    );
    adapter.execute(previewed(&adapter, LOCAL, MachineAction::Stop).unwrap());
    assert_eq!(finished(&events, &MachineAction::Stop), Ok(()));

    assert_eq!(
        remote_lane.actions(),
        [
            "install",
            "check",
            "preview Restart",
            "execute token-Restart-3"
        ]
    );
    assert_eq!(
        local_lane.actions(),
        ["preview Stop", "execute token-Stop-2"]
    );
}

/// Restart and Stop never run through `perform`, with facts known or not: only a preview the
/// person confirmed runs them, so nothing reaches the Core.
#[test]
fn perform_never_restarts_or_stops() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, Some("0.9.0")))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let unreported_lane =
        ScriptedLane::new([Err(ActionFailure::Unavailable(Unavailable::CoreTooOld))]);
    let unreported = ScriptedMachine::new(Script::Lane, Arc::clone(&unreported_lane));
    let sources = ScriptedSources::with(vec![
        known(LOCAL, "This Mac", true, &unreported),
        known(PENGUIN, "penguin", false, &machine),
    ]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    let (mut facts_seen, mut unreported_seen) = (false, false);
    until(&events, "both Machines", |event| {
        if let MachineUpdatesEvent::Changed(update) = event {
            facts_seen |= update.machine == PENGUIN && update.facts.is_some();
            unreported_seen |= update.machine == LOCAL && update.link == MachineLink::Unreported;
        }
        (facts_seen && unreported_seen).then_some(())
    });

    for (id, action) in [
        (PENGUIN, MachineAction::Restart),
        (LOCAL, MachineAction::Restart),
        (LOCAL, MachineAction::Stop),
    ] {
        adapter.perform(id, action.clone());
        assert_eq!(finished(&events, &action), Err(ActionFailure::Failed));
    }
    assert!(lane.actions().is_empty() && unreported_lane.actions().is_empty());
}

/// A preview reports what the Core itself will end, and executing it runs exactly that
/// preview's token. A preview the adapter did not issue, or one already executed, asks again.
#[test]
fn an_executed_preview_runs_the_cores_own_token() {
    let lane = ScriptedLane::new([Err(ActionFailure::Unavailable(Unavailable::CoreTooOld))]);
    lane.preview_ends(4);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    changed(&events, "the unreported Core", |update| {
        update.link == MachineLink::Unreported
    });

    let preview = adapter
        .preview(PENGUIN, MachineAction::Restart)
        .recv_blocking()
        .unwrap()
        .unwrap();
    assert_eq!(preview.sessions, 4, "the Core's own count, with no facts");
    assert_eq!(preview.action, MachineAction::Restart);

    let forged = MachinePreview {
        id: preview.id + 1,
        ..preview.clone()
    };
    adapter.execute(forged);
    assert_eq!(
        finished(&events, &MachineAction::Restart),
        Err(ActionFailure::Changed)
    );
    adapter.execute(preview.clone());
    assert_eq!(finished(&events, &MachineAction::Restart), Ok(()));
    adapter.execute(preview);
    assert_eq!(
        finished(&events, &MachineAction::Restart),
        Err(ActionFailure::Changed)
    );
    assert_eq!(
        lane.actions(),
        ["preview Restart", "execute token-Restart-4"]
    );

    let refused = adapter
        .preview(PENGUIN, MachineAction::Stop)
        .recv_blocking()
        .unwrap();
    assert_eq!(refused, Err(ActionFailure::Failed), "Stop stays local");
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

    adapter.execute(previewed(&adapter, PENGUIN, MachineAction::Restart).unwrap());
    assert_eq!(finished(&events, &MachineAction::Restart), Ok(()));
    // The Core closes its lane to restart and does not come back.
    let unconfirmed = changed(&events, "the unconfirmed restart", |update| {
        matches!(update.link, MachineLink::Unconfirmed { .. })
    });
    assert!(matches!(
        unconfirmed.link,
        MachineLink::Unconfirmed { expected, .. } if expected == version("0.9.0")
    ));
}

/// While a restart this desktop asked for has not come back, the Machine's routes retry now, at
/// most every two seconds however often its sources change, and stop once the Core answers.
#[test]
fn an_expected_restart_retries_the_machines_routes_until_the_core_returns() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, Some("0.9.0")))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    changed(&events, "the facts", |update| update.facts.is_some());

    adapter.execute(previewed(&adapter, PENGUIN, MachineAction::Restart).unwrap());
    assert_eq!(finished(&events, &MachineAction::Restart), Ok(()));
    // The Core closes its lane to restart, and its route drops.
    machine.set(Script::Offline);
    lane.answer(Err(ActionFailure::Unreachable));
    eventually("a retry of the routes", || {
        *machine.retries.lock().unwrap() == 1
    });
    for _ in 0..3 {
        sources.change();
    }
    std::thread::sleep(QUIET);
    assert_eq!(
        *machine.retries.lock().unwrap(),
        1,
        "at most every two seconds"
    );

    let mut returned = facts(1, 0, None);
    returned.boot = "boot-2".into();
    returned.facts.running = version("0.9.0");
    lane.facts.lock().unwrap().push_back(Ok(returned));
    machine.set(Script::Lane);
    sources.change();
    changed(&events, "the returned Core", |update| {
        update
            .facts
            .as_ref()
            .is_some_and(|facts| facts.running == version("0.9.0"))
    });
    machine.set(Script::Offline);
    lane.answer(Err(ActionFailure::Unreachable));
    changed(&events, "the offline Machine", |update| {
        matches!(update.link, MachineLink::Offline { .. })
    });
    std::thread::sleep(Duration::from_millis(2200));
    sources.change();
    std::thread::sleep(QUIET);
    assert_eq!(
        *machine.retries.lock().unwrap(),
        1,
        "no restart is expected now"
    );
}

/// A route change while a long poll waits abandons it and reads the Machine's reach again,
/// so a disconnected Machine stops reading as connected.
#[test]
fn a_disconnect_abandons_the_long_poll_and_reads_the_reach_again() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, None))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);
    changed(&events, "the facts", |update| update.facts.is_some());
    eventually("the long poll", || lane.count("wait") == 1);

    machine.set(Script::Offline);
    sources.change();
    changed(&events, "the disconnected Machine", |update| {
        matches!(update.link, MachineLink::Offline { .. })
    });
    std::thread::sleep(QUIET);
    assert_eq!(lane.count("wait"), 1, "no further long poll");
}

/// One Machine is listed once however many sources name it, and a Machine whose source is
/// replaced, as when its only connected route changes, is followed through the new source.
#[test]
fn one_machine_id_is_one_row_followed_through_its_current_source() {
    let old_lane = ScriptedLane::new([]);
    let old = ScriptedMachine::new(Script::Offline, Arc::clone(&old_lane));
    let new_lane = ScriptedLane::new([Ok(facts(1, 1, None))]);
    let new = ScriptedMachine::new(Script::Lane, Arc::clone(&new_lane));
    let sources = ScriptedSources::with(vec![
        known(PENGUIN, "penguin", false, &old),
        known(PENGUIN, "penguin (LAN)", false, &new),
    ]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);
    assert_eq!(listing(&events), [PENGUIN]);
    changed(&events, "the offline Machine", |update| {
        matches!(update.link, MachineLink::Offline { .. })
    });
    assert_eq!(new_lane.count("facts"), 0);

    sources.set(vec![known(PENGUIN, "penguin", false, &new)]);
    let connected = changed(&events, "facts over the new route", |update| {
        update.facts.is_some()
    });
    assert_eq!(connected.machine, PENGUIN);
    assert_eq!(new_lane.count("facts"), 1);
}

/// A Machine paired while the adapter runs, or removed, lists every Machine again.
#[test]
fn added_and_removed_machines_are_listed_again() {
    let local = ScriptedMachine::new(Script::Offline, ScriptedLane::new([]));
    let remote = ScriptedMachine::new(Script::Offline, ScriptedLane::new([]));
    let sources = ScriptedSources::with(vec![known(LOCAL, "This Mac", true, &local)]);
    let (_adapter, events) = started(&sources, RESTART_LIMIT);
    assert_eq!(listing(&events), [LOCAL]);

    sources.set(vec![
        known(LOCAL, "This Mac", true, &local),
        known(PENGUIN, "penguin", false, &remote),
    ]);
    assert_eq!(listing(&events), [LOCAL, PENGUIN]);

    sources.set(vec![known(LOCAL, "This Mac", true, &local)]);
    assert_eq!(listing(&events), [LOCAL]);
}

/// Dropping the adapter while every Machine is idle ends its watch, its coordinator and its
/// followers, with no event or change to wake them.
#[test]
fn dropping_the_adapter_ends_every_thread_while_idle() {
    let lane = ScriptedLane::new([Ok(facts(1, 0, None))]);
    let polled = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let stopped = ScriptedMachine::new(Script::Stopped, ScriptedLane::new([]));
    let sources = ScriptedSources::with(vec![
        known(LOCAL, "This Mac", true, &stopped),
        known(PENGUIN, "penguin", false, &polled),
    ]);
    let (adapter, events) = started(&sources, RESTART_LIMIT);
    changed(&events, "the stopped Core", |update| {
        update.link == MachineLink::Stopped
    });
    eventually("the long poll", || lane.count("wait") == 1);

    drop(adapter);
    assert!(sources.unwatched.load(Ordering::SeqCst), "the watch ended");
    eventually("every thread let go of its Machine", || {
        Arc::strong_count(&stopped) == 2 && Arc::strong_count(&polled) == 2
    });
    eventually("the events channel closed", || events.is_closed());
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

/// A catalog signed with the repository's public nonproduction seed. Candidate discovery uses
/// the production archive verifier, including its platform, identity and resource checks.
struct Catalog(std::path::PathBuf);

impl Catalog {
    fn new(identity: &str) -> Self {
        Self::floored(identity, "0.9.0", "0.0.0")
    }

    /// A catalog whose build is `version` with the Update Floor `floor`.
    fn floored(identity: &str, version: &str, floor: &str) -> Self {
        use ring::signature::Ed25519KeyPair;
        use sha2::{Digest, Sha256};
        use std::path::{Path, PathBuf};

        let mut random = [0; 8];
        getrandom::fill(&mut random).unwrap();
        let root = std::env::temp_dir().join(format!(
            "spaceterm-live-push-{:x}",
            u64::from_ne_bytes(random)
        ));
        let target = "x86_64-unknown-linux-gnu";
        let layout = spaceterm_core::service::Layout::for_target(target, identity).unwrap();
        let manifest = serde_json::json!({
            "schema": 1, "stapled": false, "source_sha256": null, "identity": identity, "target": target,
            "commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "version": version,
            "floor": floor, "protocols": Protocols::CURRENT,
        });
        let mut entries = vec![
            (
                PathBuf::from("build.json"),
                manifest.to_string().into_bytes(),
                0o644,
            ),
            (layout.executable(), b"embedded Core".to_vec(), 0o755),
            (
                layout.resources().join("terminfo/x/xterm-spaceterm"),
                b"terminfo".to_vec(),
                0o644,
            ),
        ];
        for resource in serde_json::from_str::<Vec<PathBuf>>(include_str!(
            "../../../../packaging/core-resources.json"
        ))
        .unwrap()
        {
            entries.push((
                layout.resource(&resource).unwrap(),
                b"resource".to_vec(),
                0o644,
            ));
        }
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut archive = tar::Builder::new(encoder);
        for (path, bytes, mode) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            archive
                .append_data(&mut header, Path::new("core").join(path), bytes.as_slice())
                .unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        let entry = root.join(target);
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(entry.join("artifact.tar.gz"), &bytes).unwrap();
        let key = Ed25519KeyPair::from_seed_unchecked(&[0x5a; 32]).unwrap();
        std::fs::write(entry.join("artifact.sig"), key.sign(&bytes).as_ref()).unwrap();
        std::fs::write(
            entry.join("artifact.sha256"),
            format!("{:x}", Sha256::digest(&bytes)),
        )
        .unwrap();
        Self(root)
    }

    fn embedded(&self, identity: &str) -> EmbeddedCores {
        EmbeddedCores::new(Some(self.0.clone()), identity)
    }
}

impl Drop for Catalog {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn a_core_without_feed_access_offers_the_verified_embedded_build() {
    use spaceterm_core::update::facts::CheckFailure;

    for identity in ["spaceterm-development", "spaceterm-preflight"] {
        let catalog = Catalog::new(identity);
        let mut observed = facts(1, 3, None);
        observed.facts.check = UpdateCheck::Disabled;
        let lane = ScriptedLane::new([Ok(observed)]);
        let script = if identity == "spaceterm-preflight" {
            Script::Incompatible
        } else {
            Script::Lane
        };
        let machine = ScriptedMachine::new(script, Arc::clone(&lane));
        let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
        let adapter = LiveMachines::new(sources, catalog.embedded(identity));
        let (sent, events) = async_channel::unbounded();
        adapter.start(sent).unwrap();
        let update = changed(&events, "the Core with no feed", |update| {
            update.facts.is_some()
        });
        assert_eq!(update.push, Some(version("0.9.0")));

        let mut failing = facts(2, 3, None);
        failing.facts.check = UpdateCheck::Failing {
            since: 1,
            last_success: None,
            reason: CheckFailure::Network,
        };
        lane.answer(Ok(failing));
        let update = changed(&events, "the failed feed check", |update| {
            update
                .facts
                .as_ref()
                .is_some_and(|facts| facts.revision == 2)
        });
        assert_eq!(update.push, Some(version("0.9.0")));
    }
}

/// A Core below the embedded build's Update Floor is offered no push across the floor, but it
/// never reads as up to date: it offers the Core's own feed install, which reaches the floor
/// through a Stepping Stone.
#[test]
fn a_core_below_the_embedded_floor_offers_its_feed_update() {
    let catalog = Catalog::floored("spaceterm-development", "1.2.2", "1.1.0");
    let mut observed = facts(1, 2, None);
    observed.facts.running = version("1.0.0");
    let lane = ScriptedLane::new([Ok(observed)]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let adapter = LiveMachines::new(sources, catalog.embedded("spaceterm-development"));
    let (sent, events) = async_channel::unbounded();
    adapter.start(sent).unwrap();
    let update = changed(&events, "the Core facts", |update| update.facts.is_some());
    assert_eq!(update.push, None, "no push crosses the floor");

    let view = MachineView::new(&update, &Dismissed::new(), crate::updates::now());
    assert_eq!(
        view.state,
        MachineState::BelowFloor {
            to: version("1.2.2")
        }
    );
    assert_eq!(
        view.sentence().as_ref(),
        "SpaceTerm Core 1.0.0 on penguin is too old to install SpaceTerm Core 1.2.2 directly. \
         Updating penguin installs the release it needs first from its update feed. Its 2 \
         Terminal Sessions keep running."
    );
    let row = view.settings_row();
    assert_eq!(row.status.as_ref(), "Older than 1.2.2");
    let button = row.button.expect("Settings offers the feed update");
    assert_eq!(button.label.as_ref(), "Update penguin");
    assert_eq!(
        button.command,
        crate::updates::presentation::Command::Machine(PENGUIN, MachineAction::Install)
    );

    adapter.perform(PENGUIN, MachineAction::Install);
    assert_eq!(finished(&events, &MachineAction::Install), Ok(()));
    assert!(
        lane.requests().contains(&"install".into()),
        "Install asks the Core to update from its own feed"
    );
}

#[test]
fn install_delivers_the_offered_build_to_local_and_paired_cores() {
    for (id, local) in [(LOCAL, true), (PENGUIN, false)] {
        let catalog = Catalog::new("spaceterm-development");
        let lane = ScriptedLane::new([Ok(facts(1, 3, None))]);
        let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
        let sources = ScriptedSources::with(vec![known(id, "Machine", local, &machine)]);
        let adapter = LiveMachines::new(sources, catalog.embedded("spaceterm-development"));
        let (sent, events) = async_channel::unbounded();
        adapter.start(sent).unwrap();
        changed(&events, "the Core facts", |update| update.facts.is_some());
        adapter.perform(id, MachineAction::Install);
        assert_eq!(finished(&events, &MachineAction::Install), Ok(()));
        assert!(
            lane.requests().contains(&"push 0.9.0".into()),
            "Install sends the embedded build instead of fetching a feed"
        );
    }
}

#[test]
fn offers_follow_pending_running_and_skipped_versions_and_survive_offline() {
    let catalog = Catalog::new("spaceterm-development");
    let lane = ScriptedLane::new([Ok(facts(1, 3, None))]);
    let machine = ScriptedMachine::new(Script::Lane, Arc::clone(&lane));
    let sources = ScriptedSources::with(vec![known(PENGUIN, "penguin", false, &machine)]);
    let adapter = LiveMachines::new(
        Arc::clone(&sources) as Arc<dyn Sources>,
        catalog.embedded("spaceterm-development"),
    );
    let (sent, events) = async_channel::unbounded();
    adapter.start(sent).unwrap();
    let update = changed(&events, "the Core facts", |update| update.facts.is_some());
    assert_eq!(update.push, Some(version("0.9.0")));

    for (index, (running, pending, skipped, platform, offer)) in [
        ("0.8.0", Some("0.8.1"), None, Platform::LinuxX86_64, true),
        ("0.8.0", Some("0.9.0"), None, Platform::LinuxX86_64, false),
        ("0.8.0", Some("1.0.0"), None, Platform::LinuxX86_64, false),
        ("0.8.0", None, Some("0.9.0"), Platform::LinuxX86_64, false),
        ("0.9.0", None, None, Platform::LinuxX86_64, false),
        ("1.0.0", None, None, Platform::LinuxX86_64, false),
        ("0.8.0", None, None, Platform::MacosArm64, false),
        ("0.8.0", None, None, Platform::LinuxX86_64, true),
    ]
    .into_iter()
    .enumerate()
    {
        let revision = index as u64 + 2;
        let mut observed = facts(revision, 3, pending);
        observed.facts.running = version(running);
        observed.facts.skipped = skipped.map(version);
        observed.facts.platform = platform;
        lane.answer(Ok(observed));
        let update = changed(&events, "the new Core facts", |update| {
            update
                .facts
                .as_ref()
                .is_some_and(|facts| facts.revision == revision)
        });
        assert_eq!(update.push, offer.then(|| version("0.9.0")));
    }
    machine.set(Script::Offline);
    lane.answer(Err(ActionFailure::Unreachable));
    let offline = changed(&events, "the offline Core", |update| {
        matches!(update.link, MachineLink::Offline { .. })
    });
    assert_eq!(offline.push, Some(version("0.9.0")));
    adapter.perform(PENGUIN, MachineAction::Install);
    assert_eq!(
        finished(&events, &MachineAction::Install),
        Err(ActionFailure::Unreachable)
    );
}
