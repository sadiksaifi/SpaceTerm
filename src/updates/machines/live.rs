//! The production Machine updates: This Mac or This Computer over the account-local service
//! lane, and each paired Machine over its own service lane.
//!
//! One follower thread per Machine reads `Facts` and then long-polls `WaitFacts`, which answers
//! on every change and at least every [`spaceterm_core::service::lane::WAIT_FACTS_LIMIT`]. Each
//! change its [`Sources`] report, such as a route connecting or disconnecting or the local
//! service changing, abandons the long poll and reads the Machine's reach again. A follower whose
//! Core it can't reach waits for the next such change. It never reads on a timer, except while a
//! restart it expects has not come back. Then it also asks the Machine's routes to retry now,
//! so the returning Core is reached without waiting out the routes' backoff. A Core whose
//! work route was refused as incompatible and whose service lane then reports a compatible work
//! protocol gets that route checked again once, however fast it returned from its update.
//!
//! A local Core that is deliberately stopped reads as [`MachineLink::Stopped`] from the service
//! manager, which needs no connection. A Core that answers `Facts` with `unsupported` predates
//! update facts and reads as [`MachineLink::Unreported`].
//!
//! Restart and Stop run only on a preview the Core issued: [`MachineUpdatesAdapter::preview`]
//! retains the Core's own token and [`MachineUpdatesAdapter::execute`] executes exactly it. The
//! Core refuses the token when its sessions or Pending Version changed since. Install pushes the
//! offered embedded Core build, or asks the Core to fetch its newest release when none is
//! offered. Check Again asks it to check (`Check`), and Start runs through the local service
//! manager, because a stopped Core accepts no connection.
//!
//! Dropping the adapter retires every thread it started and ends its sources' watches.

pub(crate) mod desktop;
pub(crate) mod service_changes;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::SharedString;
use spaceterm_core::protocol::capability::{Capabilities, Unavailable};
use spaceterm_core::protocol::version::{self as protocol, Incompatible};
use spaceterm_core::service::lane::admin;
use spaceterm_core::update::facts::{Operation, UpdateFacts, Version};

use crate::updates::push::{EmbeddedCore, EmbeddedCores};

use super::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachinePreview, MachineUpdate,
    MachineUpdatesAdapter, MachineUpdatesError, MachineUpdatesEvent, PreviewAnswer,
};

/// How long a restart may take before the Machine reads "Can't confirm". It matches how long the
/// lane follows a Core that replaces itself.
pub(crate) const RESTART_LIMIT: Duration = admin::RETURN_WAIT;
/// How often a follower retries a Core that is restarting, until [`RESTART_LIMIT`].
const RESTART_RETRY: Duration = Duration::from_secs(2);
/// The pause before a follower reads again after its long poll ended by itself, so a lane that
/// answers `Facts` but fails `WaitFacts` at once can't spin.
const REPOLL: Duration = Duration::from_secs(1);
/// The most previews kept for execution. An older one is forgotten, and executing it asks
/// again.
const RETAINED_PREVIEWS: usize = 16;

/// What one `Facts` or `WaitFacts` answer carried.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Observed {
    /// The Core run the facts belong to.
    pub(crate) boot: String,
    pub(crate) facts: UpdateFacts,
}

/// A confirmed change the lane previews and executes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaneChange {
    Restart,
    Stop,
}

/// One Machine's service lane. Every call blocks, and every failure is typed.
pub(crate) trait MachineLane: Send + Sync {
    /// `Facts`. A Core that predates update facts fails with
    /// `Unavailable(Unavailable::CoreTooOld)`.
    fn facts(&self) -> Result<Observed, ActionFailure>;
    /// `WaitFacts`: answers once the Core's `boot` or `revision` differs, or after the lane's
    /// wait limit with unchanged facts. It gives up as soon as `stale()` is true.
    fn wait_facts(
        &self,
        boot: &str,
        revision: u64,
        stale: &dyn Fn() -> bool,
    ) -> Result<Observed, ActionFailure>;
    /// `Check`.
    fn check(&self) -> Result<(), ActionFailure>;
    /// Pushes `build` when supplied, otherwise fetches from the Core's feed.
    fn install(&self, build: Option<&EmbeddedCore>) -> Result<(), ActionFailure>;
    /// `Preview` of `change`: the Core's token and what it affects.
    fn preview(&self, change: LaneChange) -> Result<admin::Preview, ActionFailure>;
    /// `Execute` of exactly `preview`, followed until it settles.
    fn execute(&self, preview: admin::Preview) -> Result<(), ActionFailure>;
}

/// How a Machine can be reached now.
pub(crate) enum Reach {
    /// This Mac's or This Computer's Core is deliberately stopped.
    Stopped,
    /// No route answers.
    Offline,
    /// Admission refused the protocol or retained state; connectivity is known.
    Incompatible(Incompatible),
    /// Its service lane may answer. `work` says whether it shares a work major.
    Lane {
        lane: Arc<dyn MachineLane>,
        work: Result<(), Incompatible>,
        /// Whether the lane belongs to a route whose Core refused this desktop's work
        /// protocol. With a compatible `work`, that Core has since been updated, and the route
        /// admits work again only once it is checked again.
        refused: bool,
    },
}

/// One Machine as its source sees it.
pub(crate) trait Machine: Send + Sync {
    /// Reads how the Machine can be reached. It may block briefly, never for a whole request
    /// limit.
    fn reach(&self) -> Reach;
    /// Starts a deliberately stopped local Core through the service manager.
    fn start(&self) -> Result<(), ActionFailure>;
    /// Asks each route to the Machine to try again now, without the backoff its earlier
    /// failures earned, because its Core is expected back from a restart or was updated out
    /// of an incompatible work protocol. A route refused as incompatible is checked once more.
    fn retry(&self);
}

/// A Machine its source knows, with every route it has.
#[derive(Clone)]
pub(crate) struct Known {
    pub(crate) id: MachineId,
    /// "This Mac", "This Computer" or the paired name.
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    pub(crate) machine: Arc<dyn Machine>,
}

/// Called after each change that may add, remove or re-reach a Machine. It returns false once
/// nobody follows the changes.
pub(crate) type Changed = Arc<dyn Fn() -> bool + Send + Sync>;

/// Ends a source's watch when dropped, without waiting for its next change.
pub(crate) type Watching = Box<dyn Send>;

/// Where the Machines come from.
pub(crate) trait Sources: Send + Sync {
    /// Every Machine now, one per Machine ID: This Mac or This Computer first, when its Core is
    /// installed, then each paired Machine.
    fn known(&self) -> Vec<Known>;
    /// Starts calling `changed` after each change, until it returns false or the returned
    /// [`Watching`] is dropped.
    fn watch(&self, changed: Changed) -> Result<Watching, MachineUpdatesError>;
}

/// The production [`MachineUpdatesAdapter`]. Construction has no effects; `start` starts the
/// followers, and dropping it ends them.
pub(crate) struct LiveMachines {
    sources: Arc<dyn Sources>,
    embedded: Arc<EmbeddedCores>,
    restart_limit: Duration,
    shared: OnceLock<Arc<Shared>>,
    watching: Mutex<Option<Watching>>,
}

impl LiveMachines {
    pub(crate) fn new(sources: Arc<dyn Sources>, embedded: EmbeddedCores) -> Self {
        Self::with_restart_limit(sources, Arc::new(embedded), RESTART_LIMIT)
    }

    pub(crate) fn with_restart_limit(
        sources: Arc<dyn Sources>,
        embedded: Arc<EmbeddedCores>,
        restart_limit: Duration,
    ) -> Self {
        Self {
            sources,
            embedded,
            restart_limit,
            shared: OnceLock::new(),
            watching: Mutex::new(None),
        }
    }

    /// Runs `work` on its own thread, which ends with `finish(outcome)`. A thread that can't
    /// start finishes with `Failed` at once.
    fn spawn_action(
        &self,
        shared: &Arc<Shared>,
        machine: MachineId,
        action: MachineAction,
        work: impl FnOnce(&Shared) -> Result<(), ActionFailure> + Send + 'static,
    ) {
        let thread_shared = Arc::clone(shared);
        let finished = action.clone();
        let spawned = std::thread::Builder::new()
            .name("spaceterm-machine-action".into())
            .spawn(move || {
                let outcome = work(&thread_shared);
                thread_shared.poke(machine);
                thread_shared.emit(MachineUpdatesEvent::Finished {
                    machine,
                    action: finished,
                    outcome,
                });
            });
        if spawned.is_err() {
            shared.emit(MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome: Err(ActionFailure::Failed),
            });
        }
    }
}

impl Drop for LiveMachines {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.get() {
            shared.close();
        }
        drop(
            self.watching
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
    }
}

impl MachineUpdatesAdapter for LiveMachines {
    fn start(
        &self,
        events: async_channel::Sender<MachineUpdatesEvent>,
    ) -> Result<(), MachineUpdatesError> {
        let shared = Arc::new(Shared {
            events,
            embedded: Arc::clone(&self.embedded),
            restart_limit: self.restart_limit,
            state: Mutex::new(State::default()),
            coordinator: Arc::new(Wake::default()),
        });
        if self.shared.set(Arc::clone(&shared)).is_err() {
            return Err(MachineUpdatesError::Unavailable);
        }
        {
            let shared = Arc::clone(&shared);
            let sources = Arc::clone(&self.sources);
            std::thread::Builder::new()
                .name("spaceterm-machines".into())
                .spawn(move || coordinate(&shared, sources.as_ref()))
                .map_err(|_| MachineUpdatesError::Unavailable)?;
        }
        let wake = Arc::clone(&shared.coordinator);
        match self.sources.watch(Arc::new(move || {
            wake.poke();
            !wake.retired()
        })) {
            Ok(watching) => {
                *self.watching.lock().unwrap_or_else(|e| e.into_inner()) = Some(watching);
                Ok(())
            }
            Err(error) => {
                shared.close();
                Err(error)
            }
        }
    }

    fn perform(&self, machine: MachineId, action: MachineAction) {
        let Some(shared) = self.shared.get() else {
            return;
        };
        let Some(target) = shared.target(machine) else {
            shared.emit(MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome: Err(ActionFailure::Unreachable),
            });
            return;
        };
        let performed = action.clone();
        self.spawn_action(shared, machine, action, move |_| act(&target, &performed));
    }

    fn preview(&self, machine: MachineId, action: MachineAction) -> PreviewAnswer {
        let (answer, answered) = async_channel::bounded(1);
        let Some(shared) = self.shared.get() else {
            let _ = answer.try_send(Err(ActionFailure::Unreachable));
            return answered;
        };
        let Some(target) = shared.target(machine) else {
            let _ = answer.try_send(Err(ActionFailure::Unreachable));
            return answered;
        };
        let shared = Arc::clone(shared);
        let spawned = std::thread::Builder::new()
            .name("spaceterm-machine-preview".into())
            .spawn({
                let answer = answer.clone();
                move || {
                    let previewed = preview(&shared, machine, &target, &action);
                    let _ = answer.try_send(previewed);
                }
            });
        if spawned.is_err() {
            let _ = answer.try_send(Err(ActionFailure::Failed));
        }
        answered
    }

    fn execute(&self, preview: MachinePreview) {
        let Some(shared) = self.shared.get() else {
            return;
        };
        let machine = preview.machine;
        let retained = shared.take_preview(&preview);
        let target = shared.target(machine);
        let action = preview.action.clone();
        self.spawn_action(shared, machine, action, move |shared| {
            // A forgotten preview, or one of a removed Machine, is confirmed by nobody now.
            let (Some(lane_preview), Some(target)) = (retained, target) else {
                return Err(ActionFailure::Changed);
            };
            let Reach::Lane { lane, .. } = target.known.machine.reach() else {
                return Err(ActionFailure::Unreachable);
            };
            run_previewed(shared, machine, lane.as_ref(), &preview, lane_preview)
        });
    }
}

/// What an action needs of its Machine, read once when the person asked.
struct Target {
    known: Known,
    /// The facts this adapter reported last, which the person saw.
    facts: Option<UpdateFacts>,
    /// The embedded Core build Install pushes, when one is offered.
    push: Option<Arc<EmbeddedCore>>,
    floor: Option<spaceterm_core::update::ordering::Floor>,
}

fn change_of(local: bool, action: &MachineAction) -> Result<LaneChange, ActionFailure> {
    match action {
        MachineAction::Restart => Ok(LaneChange::Restart),
        MachineAction::Stop if local => Ok(LaneChange::Stop),
        // Stop is This Mac's or This Computer's alone; nothing else is previewed.
        _ => Err(ActionFailure::Failed),
    }
}

/// The version a preview names for its Restart, when it names one.
fn previewed_target(preview: &admin::Preview) -> Option<Version> {
    preview
        .effects
        .to
        .as_ref()
        .and_then(|to| Version::parse(&to.version).ok())
}

fn preview(
    shared: &Shared,
    machine: MachineId,
    target: &Target,
    action: &MachineAction,
) -> Result<MachinePreview, ActionFailure> {
    let change = change_of(target.known.local, action)?;
    let Reach::Lane { lane, .. } = target.known.machine.reach() else {
        return Err(ActionFailure::Unreachable);
    };
    let lane_preview = lane.preview(change)?;
    let previewed = MachinePreview {
        machine,
        action: action.clone(),
        sessions: lane_preview.effects.ends_sessions,
        running: lane_preview
            .effects
            .from
            .as_ref()
            .and_then(|from| Version::parse(&from.version).ok()),
        target: previewed_target(&lane_preview),
        id: 0,
    };
    Ok(shared.retain_preview(previewed, lane_preview))
}

/// Executes `lane_preview`, which `preview` describes, and expects a Restart's return.
fn run_previewed(
    shared: &Shared,
    machine: MachineId,
    lane: &dyn MachineLane,
    preview: &MachinePreview,
    lane_preview: admin::Preview,
) -> Result<(), ActionFailure> {
    let restart = preview.action == MachineAction::Restart;
    if restart {
        shared.expect_restart(machine, preview.target.clone());
    }
    let outcome = lane.execute(lane_preview);
    if restart && outcome.is_err() {
        shared.forget_restart(machine);
    }
    outcome
}

fn act(target: &Target, action: &MachineAction) -> Result<(), ActionFailure> {
    match action {
        MachineAction::DismissFailure(_) => return Ok(()),
        MachineAction::Start if target.known.local => return target.known.machine.start(),
        // Restart and Stop run only through `execute`, on a preview the person confirmed. Start
        // reverses a deliberate local Stop, so it is This Mac's or This Computer's alone.
        MachineAction::Restart | MachineAction::Stop | MachineAction::Start => {
            return Err(ActionFailure::Failed);
        }
        MachineAction::Install | MachineAction::CheckAgain => {}
    }
    if *action == MachineAction::CheckAgain {
        target.known.machine.retry();
    }
    let Reach::Lane { lane, .. } = target.known.machine.reach() else {
        // Retrying routes is asynchronous. The follower reads the new reach when they answer.
        return if *action == MachineAction::CheckAgain {
            Ok(())
        } else {
            Err(ActionFailure::Unreachable)
        };
    };
    match action {
        MachineAction::Install => {
            lane.install(target.push.as_deref())
                .map_err(|failure| match (target.floor, failure) {
                    (Some(floor), ActionFailure::Failed | ActionFailure::Unreachable) => {
                        ActionFailure::NeedsFloor(floor)
                    }
                    _ => failure,
                })
        }
        _ => lane.check(),
    }
}

/// Wakes one waiting thread, or retires it.
#[derive(Default)]
struct Wake {
    state: Mutex<WakeState>,
    signal: Condvar,
}

#[derive(Default)]
struct WakeState {
    generation: u64,
    retired: bool,
}

impl Wake {
    fn lock(&self) -> std::sync::MutexGuard<'_, WakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn poke(&self) {
        self.lock().generation += 1;
        self.signal.notify_all();
    }

    fn retire(&self) {
        self.lock().retired = true;
        self.signal.notify_all();
    }

    fn retired(&self) -> bool {
        self.lock().retired
    }

    /// The current generation, or None once retired.
    fn generation(&self) -> Option<u64> {
        let state = self.lock();
        (!state.retired).then_some(state.generation)
    }

    /// Waits until a poke after `seen`, retirement or `limit`. False once retired.
    fn wait(&self, seen: u64, limit: Option<Duration>) -> bool {
        let deadline = limit.map(|limit| Instant::now() + limit);
        let mut state = self.lock();
        while !state.retired && state.generation == seen {
            match deadline {
                None => {
                    state = self.signal.wait(state).unwrap_or_else(|e| e.into_inner());
                }
                Some(deadline) => {
                    let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                        break;
                    };
                    state = self
                        .signal
                        .wait_timeout(state, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
            }
        }
        !state.retired
    }
}

struct Shared {
    embedded: Arc<EmbeddedCores>,
    events: async_channel::Sender<MachineUpdatesEvent>,
    restart_limit: Duration,
    state: Mutex<State>,
    coordinator: Arc<Wake>,
}

#[derive(Default)]
struct State {
    /// Whether the first `Machines` event went out.
    listed: bool,
    closed: bool,
    order: Vec<MachineId>,
    entries: HashMap<MachineId, Entry>,
    /// Previews the Core issued that nobody executed yet, oldest first.
    previews: VecDeque<(MachinePreview, admin::Preview)>,
    next_preview: u64,
}

struct Entry {
    known: Known,
    update: MachineUpdate,
    wake: Arc<Wake>,
    /// The Core run the latest facts belong to.
    boot: Option<String>,
    /// A restart this desktop asked for, or the Core announced, that has not come back.
    restart: Option<Awaited>,
    /// Whether the follower asked the Machine's routes to check a refused work route again
    /// since its service lane began reporting a compatible work protocol.
    rechecked: bool,
}

struct Awaited {
    /// The Pending Version, when the facts or the preview named one.
    expected: Option<Version>,
    boot: Option<String>,
    announced: bool,
    deadline: Instant,
    /// When the follower last asked the Machine's routes to retry now.
    retried: Option<Instant>,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Sends `event`. Once nobody receives, every thread retires.
    fn emit(&self, event: MachineUpdatesEvent) {
        if self.events.try_send(event).is_err() {
            self.close();
        }
    }

    /// Retires the coordinator and every follower, which wakes each wait and abandons each
    /// long poll.
    fn close(&self) {
        self.coordinator.retire();
        let mut state = self.lock();
        state.closed = true;
        for entry in state.entries.values() {
            entry.wake.retire();
        }
        state.previews.clear();
    }

    fn poke(&self, id: MachineId) {
        if let Some(entry) = self.lock().entries.get(&id) {
            entry.wake.poke();
        }
    }

    fn target(&self, id: MachineId) -> Option<Target> {
        let state = self.lock();
        let entry = state.entries.get(&id)?;
        Some(Target {
            known: entry.known.clone(),
            facts: entry.update.facts.clone(),
            floor: entry.update.facts.as_ref().and_then(|facts| {
                let floor = self.embedded.build(facts.platform)?.candidate.floor()?;
                (!floor.admits(&facts.running)).then_some(floor)
            }),
            push: entry.update.facts.as_ref().and_then(|facts| {
                entry.update.push.as_ref()?;
                self.embedded.build(facts.platform)
            }),
        })
    }

    /// The Machine's current source, which a follower reads before each reach.
    fn machine(&self, id: MachineId) -> Option<Arc<dyn Machine>> {
        Some(Arc::clone(&self.lock().entries.get(&id)?.known.machine))
    }

    fn retain_preview(&self, preview: MachinePreview, lane: admin::Preview) -> MachinePreview {
        let mut state = self.lock();
        state.next_preview += 1;
        let preview = MachinePreview {
            id: state.next_preview,
            ..preview
        };
        if state.previews.len() == RETAINED_PREVIEWS {
            state.previews.pop_front();
        }
        state.previews.push_back((preview.clone(), lane));
        preview
    }

    /// The Core's preview `preview` names, once.
    fn take_preview(&self, preview: &MachinePreview) -> Option<admin::Preview> {
        let mut state = self.lock();
        let index = state
            .previews
            .iter()
            .position(|(retained, _)| retained == preview)?;
        state.previews.remove(index).map(|(_, lane)| lane)
    }

    /// Applies `change` to `id`'s entry and reports the Machine when its update changed.
    fn publish<T>(&self, id: MachineId, change: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        let (result, sent) = {
            let mut state = self.lock();
            let entry = state.entries.get_mut(&id)?;
            let before = entry.update.clone();
            let result = change(entry);
            let sent = entry.update == before
                || self.send_locked(MachineUpdatesEvent::Changed(Box::new(entry.update.clone())));
            (result, sent)
        };
        if !sent {
            self.close();
        }
        Some(result)
    }

    /// Sends `event` while the caller holds the state lock, so events leave in the order the
    /// state changed: a Machine's changes never precede the listing that names it. False when
    /// nobody receives; the caller then closes once it releases the lock.
    fn send_locked(&self, event: MachineUpdatesEvent) -> bool {
        self.events.try_send(event).is_ok()
    }

    /// Brings the Machines in line with `known`: starts a follower for each new Machine ID,
    /// retires each removed one, hands each kept one its current routes, and wakes each kept
    /// follower to read its reach again.
    fn reconcile(self: &Arc<Self>, known: Vec<Known>) {
        let sent = {
            let mut renamed = Vec::new();
            let mut state = self.lock();
            if state.closed {
                return;
            }
            let mut order: Vec<MachineId> = Vec::new();
            let mut unique = Vec::new();
            for machine in known {
                // One Machine per Machine ID; the first source of an ID wins.
                if !order.contains(&machine.id) {
                    order.push(machine.id);
                    unique.push(machine);
                }
            }
            let removed: Vec<MachineId> = state
                .order
                .iter()
                .filter(|id| !order.contains(id))
                .copied()
                .collect();
            for id in removed {
                if let Some(entry) = state.entries.remove(&id) {
                    entry.wake.retire();
                }
            }
            let mut started = Vec::new();
            for machine in unique {
                if let Some(entry) = state.entries.get_mut(&machine.id) {
                    if entry.update.name != machine.name {
                        entry.update.name = machine.name.clone();
                        renamed.push(entry.update.clone());
                    }
                    entry.known = machine;
                    continue;
                }
                let wake = Arc::new(Wake::default());
                let entry = Entry {
                    update: MachineUpdate {
                        machine: machine.id,
                        name: machine.name.clone(),
                        local: machine.local,
                        link: MachineLink::Connected,
                        facts: None,
                        capabilities: unconnected(),
                        push: None,
                        bundled: None,
                        failed_push: None,
                    },
                    known: machine.clone(),
                    wake: Arc::clone(&wake),
                    boot: None,
                    restart: None,
                    rechecked: false,
                };
                let shared = Arc::clone(self);
                let id = machine.id;
                let followed = std::thread::Builder::new()
                    .name("spaceterm-machine".into())
                    .spawn(move || follow(&shared, id, &wake));
                if followed.is_ok() {
                    state.entries.insert(id, entry);
                    started.push(id);
                }
            }
            let order: Vec<MachineId> = order
                .into_iter()
                .filter(|id| state.entries.contains_key(id))
                .collect();
            let listing = (!state.listed || order != state.order).then(|| {
                order
                    .iter()
                    .filter_map(|id| state.entries.get(id))
                    .map(|entry| entry.update.clone())
                    .collect::<Vec<_>>()
            });
            state.listed = true;
            state.order = order;
            // A follower just started reads its reach anyway.
            let sent = match listing {
                Some(machines) => self.send_locked(MachineUpdatesEvent::Machines(machines)),
                None => renamed
                    .into_iter()
                    .all(|update| self.send_locked(MachineUpdatesEvent::Changed(Box::new(update)))),
            };
            for (id, entry) in &state.entries {
                if !started.contains(id) {
                    entry.wake.poke();
                }
            }
            sent
        };
        if !sent {
            self.close();
        }
    }

    /// The Core answered with `observed` facts.
    fn observed(&self, id: MachineId, observed: &Observed, work: Result<(), Incompatible>) {
        let limit = self.restart_limit;
        let push = self.embedded.offer(&observed.facts);
        let bundled = self.embedded.newer(&observed.facts);
        self.publish(id, |entry| {
            entry.update.push = push;
            entry.update.bundled = bundled;
            entry.update.link = match work {
                Ok(()) => MachineLink::Connected,
                Err(incompatible) => MachineLink::Incompatible(incompatible),
            };
            entry.update.capabilities = reached(work);
            entry.update.facts = Some(observed.facts.clone());
            entry.boot = Some(observed.boot.clone());
            let restarting = match &observed.facts.operation {
                Some(Operation::Restarting { to }) => Some(Some(to.clone())),
                Some(Operation::RestartingSameVersion) => Some(None),
                _ => None,
            };
            match (&mut entry.restart, restarting) {
                // The Core began a restart this desktop didn't ask for.
                (None, Some(to)) => {
                    entry.restart = Some(Awaited {
                        expected: to,
                        boot: Some(observed.boot.clone()),
                        announced: true,
                        deadline: Instant::now() + limit,
                        retried: None,
                    });
                }
                (Some(awaited), Some(_)) => {
                    awaited.announced = true;
                }
                // The announced operation settled, including a refused silent restart in the
                // same run. An explicit request keeps waiting until announced or a new run.
                (Some(awaited), None)
                    if awaited.announced || awaited.boot.as_deref() != Some(&observed.boot) =>
                {
                    entry.restart = None;
                }
                _ => {}
            }
        });
    }

    /// The Core's lane admits this desktop but reports no update facts.
    fn unreported(&self, id: MachineId, work: Result<(), Incompatible>) {
        self.publish(id, |entry| {
            entry.update.link = MachineLink::Unreported;
            entry.update.capabilities = reached(work);
            entry.update.facts = None;
            entry.update.push = None;
            entry.update.bundled = None;
            entry.restart = None;
        });
    }

    /// The local Core is deliberately stopped.
    fn stopped(&self, id: MachineId) {
        self.publish(id, |entry| {
            entry.update.link = MachineLink::Stopped;
            entry.update.capabilities = unconnected();
            entry.restart = None;
            if let Some(facts) = &mut entry.update.facts {
                facts.sessions = 0;
                facts.operation = None;
            }
        });
    }

    /// Nothing answered `machine`. Returns how long to wait before trying again, while a
    /// restart is expected. Until the restart comes back, it also asks the Machine's routes to
    /// retry now, at most every [`RESTART_RETRY`], so a Core that returns within seconds is
    /// reached within seconds instead of on the routes' backoff.
    fn unreachable(&self, id: MachineId, machine: &dyn Machine) -> Option<Duration> {
        let (limit, retry) = self.publish(id, Self::unreached)?;
        if retry {
            machine.retry();
        }
        limit
    }

    /// The Machine of `entry` did not answer. Returns how long to wait before trying again and
    /// whether to retry its routes now.
    fn unreached(entry: &mut Entry) -> (Option<Duration>, bool) {
        entry.update.capabilities = unconnected();
        if let Some(awaited) = &mut entry.restart {
            let now = Instant::now();
            if let Some(left) = awaited.deadline.checked_duration_since(now) {
                let retry = awaited
                    .retried
                    .is_none_or(|at| now.duration_since(at) >= RESTART_RETRY);
                if retry {
                    awaited.retried = Some(now);
                }
                return (Some(left.min(RESTART_RETRY)), retry);
            }
            if let Some(expected) = awaited.expected.clone() {
                entry.update.link = MachineLink::Unconfirmed {
                    expected,
                    since: crate::updates::now(),
                };
                entry.restart = None;
                return (None, false);
            }
            entry.restart = None;
        }
        match entry.update.link {
            // "Can't confirm" stays until the Core answers again.
            MachineLink::Unconfirmed { .. } | MachineLink::Offline { .. } => {}
            _ => {
                entry.update.link = MachineLink::Offline {
                    since: crate::updates::now(),
                };
            }
        }
        (None, false)
    }

    /// This desktop asked `id` to restart into `expected`: until the Core answers from a new
    /// run, an unreachable lane reads as restarting, not offline.
    fn expect_restart(&self, id: MachineId, expected: Option<Version>) {
        let limit = self.restart_limit;
        self.publish(id, |entry| {
            entry.restart = Some(Awaited {
                expected: expected.or_else(|| {
                    entry
                        .update
                        .facts
                        .as_ref()
                        .and_then(|facts| facts.pending.as_ref())
                        .map(|pending| pending.version.clone())
                }),
                boot: entry.boot.clone(),
                announced: false,
                deadline: Instant::now() + limit,
                retried: None,
            });
        });
    }

    fn forget_restart(&self, id: MachineId) {
        self.publish(id, |entry| entry.restart = None);
    }

    /// The follower read `id`'s reach. When the service lane of a route refused as
    /// incompatible reports a compatible work protocol, the Core was updated, so the
    /// Machine's routes check the refused route again with its saved pairing. That happens
    /// once each time the reach starts reporting so, whether or not the Machine was seen
    /// offline in between, and never in a loop: a route still refused waits for the next
    /// compatibility change, or for Reconnect.
    fn compatible_again(&self, id: MachineId, machine: &dyn Machine, reach: &Reach) {
        let readmitted = matches!(
            reach,
            Reach::Lane {
                work: Ok(()),
                refused: true,
                ..
            }
        );
        let recheck = {
            let mut state = self.lock();
            let Some(entry) = state.entries.get_mut(&id) else {
                return;
            };
            let recheck = readmitted && !entry.rechecked;
            entry.rechecked = readmitted;
            recheck
        };
        if recheck {
            machine.retry();
        }
    }
}

/// Keeps the Machines in line with `sources` until the adapter closes.
fn coordinate(shared: &Arc<Shared>, sources: &dyn Sources) {
    while let Some(seen) = shared.coordinator.generation() {
        shared.reconcile(sources.known());
        if !shared.coordinator.wait(seen, None) {
            break;
        }
    }
    shared.close();
}

/// Follows one Machine until it is removed or the adapter closes. Each iteration reads the
/// Machine's current source, so a route that comes or goes takes effect at the next change.
fn follow(shared: &Shared, id: MachineId, wake: &Wake) {
    while let Some(seen) = wake.generation() {
        let Some(machine) = shared.machine(id) else {
            return;
        };
        let reach = machine.reach();
        shared.compatible_again(id, machine.as_ref(), &reach);
        let limit = match reach {
            Reach::Stopped => {
                shared.stopped(id);
                None
            }
            Reach::Offline => shared.unreachable(id, machine.as_ref()),
            Reach::Incompatible(incompatible) => {
                shared.publish(id, |entry| {
                    entry.update.link = MachineLink::Incompatible(incompatible);
                    entry.update.capabilities = reached(Err(incompatible));
                    entry.restart = None;
                });
                None
            }
            Reach::Lane { lane, work, .. } => match lane.facts() {
                Ok(observed) => {
                    shared.observed(id, &observed, work);
                    let stale = || wake.generation() != Some(seen);
                    let mut current = observed;
                    let ended_by_itself = loop {
                        if stale() {
                            break false;
                        }
                        match lane.wait_facts(&current.boot, current.facts.revision, &stale) {
                            Ok(next) => {
                                shared.observed(id, &next, work);
                                current = next;
                            }
                            Err(_) => break !stale(),
                        }
                    };
                    // A change the sources reported reads the reach again at once. A lane that
                    // ended by itself, as when the Core stops or restarts, is read after a pause
                    // or the next change.
                    if ended_by_itself {
                        let Some(now) = wake.generation() else {
                            return;
                        };
                        if !wake.wait(now, Some(REPOLL)) {
                            return;
                        }
                    }
                    continue;
                }
                Err(ActionFailure::Unavailable(Unavailable::CoreTooOld)) => {
                    shared.unreported(id, work);
                    None
                }
                Err(ActionFailure::Unavailable(Unavailable::ClientTooOld)) => {
                    shared.publish(id, |entry| {
                        entry.update.link = MachineLink::Incompatible(Incompatible::CallerTooOld);
                        entry.restart = None;
                    });
                    None
                }
                Err(_) => shared.unreachable(id, machine.as_ref()),
            },
        };
        if !wake.wait(seen, limit) {
            return;
        }
    }
}

fn unconnected() -> Capabilities {
    Capabilities::new(
        Err(Unavailable::NotConnected),
        Err(Unavailable::NotConnected),
    )
}

/// What a Machine whose service lane admitted this desktop can do.
fn reached(work: Result<(), Incompatible>) -> Capabilities {
    Capabilities::new(
        work.map(|()| protocol::WORK)
            .map_err(|incompatible| match incompatible {
                Incompatible::ListenerTooOld => Unavailable::CoreTooOld,
                Incompatible::CallerTooOld => Unavailable::ClientTooOld,
            }),
        Ok(protocol::SERVICE),
    )
}

/// The typed failure a lane or ledger code names.
pub(crate) fn failure_of_code(code: &str) -> ActionFailure {
    match code {
        "clock_behind" => ActionFailure::ClockBehind,
        "unsupported" => ActionFailure::Unavailable(Unavailable::CoreTooOld),
        "service_busy" | "capacity" | "activation_unresolved" => ActionFailure::Busy,
        "changed" | "candidate_changed" => ActionFailure::Changed,
        "login_item_off" => ActionFailure::LoginItemOff,
        "disk_full" => ActionFailure::DiskFull,
        "feed_unreachable"
        | "proxy_unreachable"
        | "unreachable"
        | "not_running"
        | "stopped"
        | "network_off"
        | "service_unavailable" => ActionFailure::Unreachable,
        _ => ActionFailure::Failed,
    }
}
