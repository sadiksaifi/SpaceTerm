//! The production Machine updates: This Mac or This Computer over the account-local service
//! lane, and each paired Machine over its own service lane.
//!
//! One follower thread per Machine reads `Facts` and then long-polls `WaitFacts`, which answers
//! on every change and at least every [`spaceterm_core::service::lane::WAIT_FACTS_LIMIT`]. A
//! follower whose Core it can't reach waits for its [`Sources`] to report a change: the local
//! service's directories or Core process, or a paired route's connection state. It never reads
//! on a timer, except while a restart it expects has not come back.
//!
//! A local Core that is deliberately stopped reads as [`MachineLink::Stopped`] from the service
//! manager, which needs no connection. A Core that answers `Facts` with `unsupported` predates
//! update facts and reads as [`MachineLink::Unreported`].
//!
//! Each action runs on its own thread and ends with one [`MachineUpdatesEvent::Finished`]:
//! Install pushes the offered embedded Core build, or asks the Core to fetch its newest release
//! (`StageFetch`) when none is offered. Check Again asks it to check (`Check`), Restart and Stop
//! preview and execute the change over the lane, and Start runs
//! through the local service manager, because a stopped Core accepts no connection.

pub(crate) mod desktop;
pub(crate) mod service_changes;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gpui::SharedString;
use spaceterm_core::protocol::capability::{Capabilities, Unavailable};
use spaceterm_core::protocol::version::{self as protocol, Incompatible};
use spaceterm_core::update::facts::{Operation, UpdateFacts, Version};

use crate::updates::push::{EmbeddedCore, EmbeddedCores};

use super::{
    ActionFailure, MachineAction, MachineId, MachineLink, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesError, MachineUpdatesEvent,
};

/// How long a restart may take before the Machine reads "Can't confirm". It matches how long the
/// lane follows a Core that replaces itself.
pub(crate) const RESTART_LIMIT: Duration = spaceterm_core::service::lane::admin::RETURN_WAIT;
/// How often a follower retries a Core that is restarting, until [`RESTART_LIMIT`].
const RESTART_RETRY: Duration = Duration::from_secs(2);
/// The pause before a follower reads again after its long poll ended, so a lane that answers
/// `Facts` but fails `WaitFacts` at once can't spin.
const REPOLL: Duration = Duration::from_secs(1);

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
    /// wait limit with unchanged facts.
    fn wait_facts(&self, boot: &str, revision: u64) -> Result<Observed, ActionFailure>;
    /// `Check`.
    fn check(&self) -> Result<(), ActionFailure>;
    /// Pushes `build` when supplied, otherwise fetches from the Core's feed.
    fn install(&self, build: Option<&EmbeddedCore>) -> Result<(), ActionFailure>;
    /// Previews `change`, executes it and follows it until it settles. `sessions` is the
    /// session count the person confirmed; a preview that ends a different number fails with
    /// `Changed` and executes nothing.
    fn change(&self, change: LaneChange, sessions: Option<u32>) -> Result<(), ActionFailure>;
}

/// How a Machine can be reached now.
pub(crate) enum Reach {
    /// This Mac's or This Computer's Core is deliberately stopped.
    Stopped,
    /// No route answers.
    Offline,
    /// Its service lane may answer. `work` says whether it shares a work major.
    Lane {
        lane: Arc<dyn MachineLane>,
        work: Result<(), Incompatible>,
    },
}

/// One Machine as its source sees it.
pub(crate) trait Machine: Send + Sync {
    /// Reads how the Machine can be reached. It may block briefly, never for a whole request
    /// limit.
    fn reach(&self) -> Reach;
    /// Starts a deliberately stopped local Core through the service manager.
    fn start(&self) -> Result<(), ActionFailure>;
}

/// A Machine its source knows.
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

/// Where the Machines come from.
pub(crate) trait Sources: Send + Sync {
    /// Every Machine now: This Mac or This Computer first, when its Core is installed, then each
    /// paired Machine.
    fn known(&self) -> Vec<Known>;
    /// Starts calling `changed` after each change, until it returns false.
    fn watch(&self, changed: Changed) -> Result<(), MachineUpdatesError>;
}

/// The production [`MachineUpdatesAdapter`]. Construction has no effects; `start` starts the
/// followers.
pub(crate) struct LiveMachines {
    sources: Arc<dyn Sources>,
    embedded: Arc<EmbeddedCores>,
    restart_limit: Duration,
    shared: OnceLock<Arc<Shared>>,
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
        }
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
        let watched = self.sources.watch(Arc::new(move || {
            wake.poke();
            !wake.retired()
        }));
        if watched.is_err() {
            shared.close();
        }
        watched
    }

    fn perform(&self, machine: MachineId, action: MachineAction) {
        let Some(shared) = self.shared.get() else {
            return;
        };
        let shared = Arc::clone(shared);
        let finished = {
            let shared = Arc::clone(&shared);
            let action = action.clone();
            move |outcome| {
                shared.emit(MachineUpdatesEvent::Finished {
                    machine,
                    action,
                    outcome,
                });
            }
        };
        let Some(target) = shared.target(machine) else {
            finished(Err(ActionFailure::Unreachable));
            return;
        };
        let performed = action.clone();
        let spawned = std::thread::Builder::new()
            .name("spaceterm-machine-action".into())
            .spawn(move || {
                let outcome = act(&shared, machine, &target, &performed);
                target.wake.poke();
                finished(outcome);
            });
        if spawned.is_err() {
            // The report went with the closure the failed spawn dropped.
            if let Some(shared) = self.shared.get() {
                shared.emit(MachineUpdatesEvent::Finished {
                    machine,
                    action,
                    outcome: Err(ActionFailure::Failed),
                });
            }
        }
    }
}

/// What an action needs of its Machine, read once when the person asked.
struct Target {
    known: Known,
    wake: Arc<Wake>,
    /// The sessions the facts the person saw reported.
    sessions: Option<u32>,
    push: Option<Arc<EmbeddedCore>>,
}

fn act(
    shared: &Shared,
    id: MachineId,
    target: &Target,
    action: &MachineAction,
) -> Result<(), ActionFailure> {
    match action {
        MachineAction::DismissFailure(_) => return Ok(()),
        MachineAction::Start if target.known.local => return target.known.machine.start(),
        // Start reverses a deliberate local Stop. Stop is This Mac's or This Computer's alone.
        MachineAction::Start | MachineAction::Stop if !target.known.local => {
            return Err(ActionFailure::Failed);
        }
        _ => {}
    }
    let Reach::Lane { lane, .. } = target.known.machine.reach() else {
        return Err(ActionFailure::Unreachable);
    };
    match action {
        MachineAction::Install => lane.install(target.push.as_deref()),
        MachineAction::CheckAgain => lane.check(),
        MachineAction::Restart => {
            shared.expect_restart(id);
            let outcome = lane.change(LaneChange::Restart, target.sessions);
            if outcome.is_err() {
                shared.forget_restart(id);
            }
            outcome
        }
        MachineAction::Stop => lane.change(LaneChange::Stop, target.sessions),
        MachineAction::Start | MachineAction::DismissFailure(_) => Ok(()),
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
    order: Vec<MachineId>,
    entries: HashMap<MachineId, Entry>,
}

struct Entry {
    known: Known,
    update: MachineUpdate,
    wake: Arc<Wake>,
    /// The Core run the latest facts belong to.
    boot: Option<String>,
    /// A restart this desktop asked for, or the Core announced, that has not come back.
    restart: Option<Awaited>,
}

struct Awaited {
    /// The Pending Version, when the facts named one.
    expected: Option<Version>,
    /// The run that restarts. Facts from another run end the wait.
    boot: Option<String>,
    deadline: Instant,
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

    fn close(&self) {
        self.coordinator.retire();
        for entry in self.lock().entries.values() {
            entry.wake.retire();
        }
    }

    fn target(&self, id: MachineId) -> Option<Target> {
        let state = self.lock();
        let entry = state.entries.get(&id)?;
        Some(Target {
            known: entry.known.clone(),
            wake: Arc::clone(&entry.wake),
            sessions: entry.update.facts.as_ref().map(|facts| facts.sessions),
            push: entry.update.facts.as_ref().and_then(|facts| {
                entry.update.push.as_ref()?;
                self.embedded.build(facts.platform)
            }),
        })
    }

    /// Applies `change` to `id`'s entry and reports the Machine when its update changed.
    fn publish<T>(&self, id: MachineId, change: impl FnOnce(&mut Entry) -> T) -> Option<T> {
        let (result, changed) = {
            let mut state = self.lock();
            let entry = state.entries.get_mut(&id)?;
            let before = entry.update.clone();
            let result = change(entry);
            let changed = (entry.update != before).then(|| entry.update.clone());
            (result, changed)
        };
        if let Some(update) = changed {
            self.emit(MachineUpdatesEvent::Changed(Box::new(update)));
        }
        Some(result)
    }

    /// Brings the Machines in line with `known`: starts a follower for each new one, retires
    /// each removed one, and wakes every follower to read its reach again.
    fn reconcile(self: &Arc<Self>, known: Vec<Known>) {
        let mut renamed = Vec::new();
        let listing = {
            let mut state = self.lock();
            let order: Vec<MachineId> = known.iter().map(|known| known.id).collect();
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
            for machine in known {
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
                    },
                    known: machine.clone(),
                    wake: Arc::clone(&wake),
                    boot: None,
                    restart: None,
                };
                let shared = Arc::clone(self);
                let id = machine.id;
                let followed = std::thread::Builder::new()
                    .name("spaceterm-machine".into())
                    .spawn(move || follow(&shared, id, machine.machine.as_ref(), &wake));
                if followed.is_ok() {
                    state.entries.insert(id, entry);
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
            for entry in state.entries.values() {
                entry.wake.poke();
            }
            listing
        };
        match listing {
            Some(machines) => self.emit(MachineUpdatesEvent::Machines(machines)),
            None => {
                for update in renamed {
                    self.emit(MachineUpdatesEvent::Changed(Box::new(update)));
                }
            }
        }
    }

    /// The Core answered with `observed` facts.
    fn observed(&self, id: MachineId, observed: &Observed, work: Result<(), Incompatible>) {
        let limit = self.restart_limit;
        let push = self.embedded.offer(&observed.facts);
        self.publish(id, |entry| {
            entry.update.push = push;
            entry.update.link = match work {
                Ok(()) => MachineLink::Connected,
                Err(incompatible) => MachineLink::Incompatible(incompatible),
            };
            entry.update.capabilities = reached(work);
            entry.update.facts = Some(observed.facts.clone());
            entry.boot = Some(observed.boot.clone());
            let restarting = match &observed.facts.operation {
                Some(Operation::Restarting { to }) => Some(to.clone()),
                _ => None,
            };
            match (&entry.restart, restarting) {
                // The Core began a restart this desktop didn't ask for.
                (None, Some(to)) => {
                    entry.restart = Some(Awaited {
                        expected: Some(to),
                        boot: Some(observed.boot.clone()),
                        deadline: Instant::now() + limit,
                    });
                }
                // A new run answered, so the restart came back. A version it didn't start on
                // reaches the facts as their last failure.
                (Some(awaited), None) if awaited.boot.as_deref() != Some(&observed.boot) => {
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

    /// Nothing answered. Returns how long to wait before trying again, while a restart is
    /// expected.
    fn unreachable(&self, id: MachineId) -> Option<Duration> {
        self.publish(id, |entry| {
            entry.update.capabilities = unconnected();
            if let Some(awaited) = &entry.restart {
                if let Some(left) = awaited.deadline.checked_duration_since(Instant::now()) {
                    return Some(left.min(RESTART_RETRY));
                }
                if let Some(expected) = awaited.expected.clone() {
                    entry.update.link = MachineLink::Unconfirmed {
                        expected,
                        since: crate::updates::now(),
                    };
                    entry.restart = None;
                    return None;
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
            None
        })
        .flatten()
    }

    /// This desktop asked `id` to restart: until the Core answers from a new run, an
    /// unreachable lane reads as restarting, not offline.
    fn expect_restart(&self, id: MachineId) {
        let limit = self.restart_limit;
        self.publish(id, |entry| {
            entry.restart = Some(Awaited {
                expected: entry
                    .update
                    .facts
                    .as_ref()
                    .and_then(|facts| facts.pending.as_ref())
                    .map(|pending| pending.version.clone()),
                boot: entry.boot.clone(),
                deadline: Instant::now() + limit,
            });
        });
    }

    fn forget_restart(&self, id: MachineId) {
        self.publish(id, |entry| entry.restart = None);
    }
}

/// Keeps the Machines in line with `sources` until nobody receives the events.
fn coordinate(shared: &Arc<Shared>, sources: &dyn Sources) {
    while let Some(seen) = shared.coordinator.generation() {
        shared.reconcile(sources.known());
        if !shared.coordinator.wait(seen, None) {
            break;
        }
    }
    shared.close();
}

/// Follows one Machine until it is removed or nobody receives the events.
fn follow(shared: &Shared, id: MachineId, machine: &dyn Machine, wake: &Wake) {
    while let Some(seen) = wake.generation() {
        let limit = match machine.reach() {
            Reach::Stopped => {
                shared.stopped(id);
                None
            }
            Reach::Offline => shared.unreachable(id),
            Reach::Lane { lane, work } => match lane.facts() {
                Ok(observed) => {
                    shared.observed(id, &observed, work);
                    let mut current = observed;
                    while wake.generation().is_some() {
                        match lane.wait_facts(&current.boot, current.facts.revision) {
                            Ok(next) => {
                                shared.observed(id, &next, work);
                                current = next;
                            }
                            Err(_) => break,
                        }
                    }
                    // The lane ended, as it does when the Core stops or restarts. Read again
                    // after a pause, or as soon as the sources report a change.
                    let Some(seen) = wake.generation() else {
                        return;
                    };
                    if !wake.wait(seen, Some(REPOLL)) {
                        return;
                    }
                    continue;
                }
                Err(ActionFailure::Unavailable(Unavailable::CoreTooOld)) => {
                    shared.unreported(id, work);
                    None
                }
                Err(_) => shared.unreachable(id),
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
        "unsupported" => ActionFailure::Unavailable(Unavailable::CoreTooOld),
        "service_busy" | "capacity" | "activation_unresolved" => ActionFailure::Busy,
        "changed" | "candidate_changed" => ActionFailure::Changed,
        "unreachable" | "not_running" | "stopped" | "network_off" | "service_unavailable" => {
            ActionFailure::Unreachable
        }
        _ => ActionFailure::Failed,
    }
}
