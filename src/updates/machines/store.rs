//! The application's one view of Machine updates: it starts the composed
//! [`MachineUpdatesAdapter`], keeps the latest [`MachineUpdate`] per Machine and the failure
//! notices dismissed on this desktop, records each finished update in Recent, and performs
//! actions. Observers of [`MachineUpdatesService`] hear every change. [`MachineUpdatesNotice`]
//! reports how each action a surface performed finished, so that surface, and only it, can
//! explain a failure.
//!
//! It installs the Core build bundled in this desktop on every Machine it offers one to, with
//! no click: once per Machine and offered version in each run, while the Machine's lane answers
//! and its Core runs no update operation. An install ends no Terminal Session. A failed one is
//! not retried automatically and raises no notice. It stays in the Machine's `failed_push`,
//! because a push that breaks off or runs out of disk space records nothing in the Core's
//! facts, so every surface explains it and offers Try Again.
//!
//! It also marks each Machine with what only this desktop knows: the check or install a person
//! started here that the Core has not ended, and when the Core's own download first failed while
//! the Core keeps trying. The presentation shows the first and keeps quiet about the second.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use spaceterm_core::update::facts::{FailureKind, InstallFailure, Version};

use super::{
    ActionFailure, Asked, FailedPush, MachineAction, MachineId, MachineLink, MachinePreview,
    MachineUpdate, MachineUpdatesAdapter, MachineUpdatesEvent,
};
use crate::updates::presentation::{self, Approval, Confirmation, Dismissed};

/// How long Check Again reads as Checking on a Machine no route reached, while its routes try
/// again.
pub(crate) const RECONNECT_WAIT: Duration = Duration::from_secs(10);

/// A check or install a person started, until the Core's facts show it ended.
struct AskedWork {
    machine: MachineId,
    asked: Asked,
    phase: Phase,
    /// Names this work to the timer that ends its reconnect wait.
    serial: u64,
}

enum Phase {
    /// The request has not answered.
    Requested,
    /// The request answered. The mark stays while the Core's facts still report an operation,
    /// because the answer and the facts that end the work arrive in either order.
    Answered,
    /// No route reached the Machine, so the check answered at once and its routes try again.
    /// The mark stays until the link changes from this one or [`RECONNECT_WAIT`] ends.
    Reconnecting(MachineLink),
}

/// A Machine's own failed download while its Core tries again by itself.
struct Retrying {
    /// The ID of the latest failure, which each try that fails replaces.
    failure: u64,
    /// When this desktop first saw the download fail, on its own clock. None when a person's
    /// own try from this desktop failed, or when the failure began unseen, before this run or
    /// while the Machine was out of reach, so that failure shows at once.
    since: Option<u64>,
}

/// Where the person asked for an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The title-bar control, its alerts and notifications.
    Window,
    /// The menu bar extra or the Linux status item.
    StatusMenu,
    /// The Settings window owns its confirmation and failure alerts.
    Settings,
}

/// How an action a surface performed finished.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineUpdatesNotice {
    pub(crate) machine: MachineId,
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    pub(crate) action: MachineAction,
    pub(crate) surface: Surface,
    pub(crate) outcome: Result<(), ActionFailure>,
}

/// What a Restart or Stop needs next, from the Cores' own previews.
enum Prepared {
    /// Nothing asks: execute these previews.
    Run(Vec<MachinePreview>),
    /// Ask with this confirmation first; approving it executes these previews.
    Ask(Confirmation, Vec<MachinePreview>),
    /// Nothing is left to run: every preview failed, and its surface heard why, or the
    /// approved work is gone.
    Nothing,
}

/// Shows a confirmation on a surface and passes on whether the person approved it.
pub(crate) type Ask = Rc<dyn Fn(Confirmation, Box<dyn FnOnce(bool, &mut App)>, &mut App)>;

/// Restarts or stops `actions`' Machines only on their Cores' own previews: each Core previews
/// first, the person confirms the sessions that Core reports through `ask`, and exactly those
/// previews execute. A Machine whose Core reports no facts is previewed like any other. A
/// preview that fails reports to `surface` for its Machine alone; the others still ask and run.
/// `always` asks even when no session closes, as after a Core refused a preview: a refused
/// action never runs again without a fresh confirmation. An approval that no longer holds
/// against the facts previews again, and keeps only the work the person approved: a restart
/// into a Pending Version that is gone now is dropped, and one into another version asks.
pub(crate) fn request(
    actions: Vec<(MachineId, MachineAction)>,
    always: bool,
    surface: Surface,
    ask: Ask,
    cx: &mut App,
) {
    request_approved(actions, always, None, surface, ask, cx);
}

fn request_approved(
    actions: Vec<(MachineId, MachineAction)>,
    always: bool,
    stale: Option<Approval>,
    surface: Surface,
    ask: Ask,
    cx: &mut App,
) {
    let Some(store) = MachineUpdates::global(cx) else {
        return;
    };
    let prepared = store.update(cx, |store, cx| {
        store.prepare(&actions, always, stale, surface, cx)
    });
    cx.spawn(async move |cx| {
        let prepared = prepared.await;
        cx.update(|cx| match prepared {
            Prepared::Nothing => {}
            Prepared::Run(previews) => execute_all(previews, surface, cx),
            Prepared::Ask(confirmation, previews) => {
                let approval = confirmation.approval.clone();
                let again = Rc::clone(&ask);
                let answered = Box::new(move |approved: bool, cx: &mut App| {
                    if !approved {
                        return;
                    }
                    let holds = MachineUpdates::global(cx)
                        .is_some_and(|store| approval.holds(store.read(cx).machines()));
                    if holds {
                        execute_all(previews, surface, cx);
                    } else {
                        request_approved(actions, false, Some(approval), surface, again, cx);
                    }
                });
                ask(confirmation, answered, cx);
            }
        });
    })
    .detach();
}

fn execute_all(previews: Vec<MachinePreview>, surface: Surface, cx: &mut App) {
    if let Some(store) = MachineUpdates::global(cx) {
        store.update(cx, |store, _| {
            for preview in previews {
                store.execute(preview, surface);
            }
        });
    }
}

pub(crate) struct MachineUpdates {
    adapter: Rc<dyn MachineUpdatesAdapter>,
    machines: Vec<MachineUpdate>,
    dismissed: Dismissed,
    /// Actions performed that have not finished, oldest first.
    performed: Vec<(MachineId, MachineAction, Surface)>,
    /// Each check or install a person started that has not ended, oldest first.
    asked: Vec<AskedWork>,
    /// How many checks and installs people started, which names the next one.
    serial: u64,
    retrying: HashMap<MachineId, Retrying>,
    /// Machines where a person's check or install answered, with the facts revision then. The
    /// next revision is that work's result, so a failure it records is the person's own.
    tried: HashMap<MachineId, Option<u64>>,
    /// Machines in reach whose facts this run has received. A failure in the first facts of a
    /// Machine, or in its first after it was out of reach, began unseen.
    heard: HashSet<MachineId>,
    /// Each bundled Core version installed automatically in this run, by Machine.
    offered: HashSet<(MachineId, Version)>,
    /// Machines whose automatic install has not finished, and the version it pushes. Its
    /// finish reaches no surface, and a failure stays in the Machine's `failed_push`.
    installing: Vec<(MachineId, Version)>,
    /// Whether the desktop's startup check may still find a newer release of this desktop.
    startup_check: bool,
    _events: Task<()>,
}

#[derive(Clone)]
pub(crate) struct MachineUpdatesService(pub(crate) Entity<MachineUpdates>);
impl Global for MachineUpdatesService {}

impl EventEmitter<MachineUpdatesNotice> for MachineUpdates {}

impl MachineUpdates {
    /// Starts `adapter` and installs the service. An adapter that cannot start installs nothing,
    /// so every surface shows no Machine updates.
    pub(crate) fn install(adapter: Rc<dyn MachineUpdatesAdapter>, cx: &mut App) {
        let (sender, receiver) = async_channel::unbounded();
        if adapter.start(sender).is_err() {
            return;
        }
        let entity = cx.new(|cx: &mut Context<Self>| {
            let events = cx.spawn(async move |this, cx| {
                while let Ok(event) = receiver.recv().await {
                    if this.update(cx, |this, cx| this.receive(event, cx)).is_err() {
                        return;
                    }
                }
            });
            Self {
                adapter,
                machines: Vec::new(),
                dismissed: Dismissed::new(),
                performed: Vec::new(),
                asked: Vec::new(),
                serial: 0,
                retrying: HashMap::new(),
                tried: HashMap::new(),
                heard: HashSet::new(),
                offered: HashSet::new(),
                installing: Vec::new(),
                startup_check: false,
                _events: events,
            }
        });
        cx.set_global(MachineUpdatesService(entity.clone()));
        if let Some(updates) = cx
            .try_global::<crate::updates::UpdateService>()
            .map(|service| service.0.clone())
        {
            entity.update(cx, |this, cx| {
                this.startup_check = true;
                cx.observe(&updates, |this, _, cx| this.install_bundled(cx))
                    .detach();
            });
            // A Stepping Stone finds its next desktop release, which holds back automatic Core
            // pushes, as early as possible.
            updates.update(cx, |updates, cx| updates.check(false, cx));
        }
    }

    pub(crate) fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<MachineUpdatesService>()
            .map(|service| service.0.clone())
    }

    pub(crate) fn machines(&self) -> &[MachineUpdate] {
        &self.machines
    }

    pub(crate) fn dismissed(&self) -> &Dismissed {
        &self.dismissed
    }

    /// Asks the adapter to perform `action` for `surface`. Dismissing a failure retires it here
    /// at once.
    pub(crate) fn perform(
        &mut self,
        machine: MachineId,
        action: MachineAction,
        surface: Surface,
        cx: &mut Context<Self>,
    ) {
        let asked = match action {
            MachineAction::DismissFailure(id) => {
                self.dismissed.insert((machine, id));
                None
            }
            MachineAction::CheckAgain => Some(Asked::Check),
            MachineAction::Install => Some(Asked::Install),
            MachineAction::Restart | MachineAction::Start | MachineAction::Stop => None,
        };
        if let Some(asked) = asked {
            self.serial += 1;
            self.asked.push(AskedWork {
                machine,
                asked,
                phase: Phase::Requested,
                serial: self.serial,
            });
        }
        self.mark();
        cx.notify();
        self.performed.push((machine, action.clone(), surface));
        self.adapter.perform(machine, action);
    }

    /// The request of a check or install a person started answered with `outcome`.
    fn answer(
        &mut self,
        machine: MachineId,
        action: &MachineAction,
        outcome: &Result<(), ActionFailure>,
        cx: &mut Context<Self>,
    ) {
        let asked = match action {
            MachineAction::CheckAgain => Asked::Check,
            MachineAction::Install => Asked::Install,
            _ => return,
        };
        let Some(index) = self.asked.iter().position(|work| {
            work.machine == machine && work.asked == asked && matches!(work.phase, Phase::Requested)
        }) else {
            return;
        };
        let update = self.machines.iter().find(|known| known.machine == machine);
        let revision = update
            .and_then(|update| update.facts.as_ref())
            .map(|facts| facts.revision);
        // A request the Core refused, or that never reached it, ran no work of the person's.
        // An operation the Core's facts report, and the result that follows, are the Core's own.
        if matches!(
            outcome,
            Err(ActionFailure::Busy
                | ActionFailure::Unavailable(_)
                | ActionFailure::Unreachable
                | ActionFailure::RelaunchRequired
                | ActionFailure::Changed
                | ActionFailure::LoginItemOff)
        ) {
            self.asked.remove(index);
            return;
        }
        self.tried.insert(machine, revision);
        // This Mac or This Computer has no route to try again.
        let unreachable = update
            .filter(|update| !update.local)
            .map(|update| &update.link)
            .filter(|link| !matches!(link, MachineLink::Connected | MachineLink::Incompatible(_)));
        let work = &mut self.asked[index];
        work.phase = match unreachable {
            Some(link) if asked == Asked::Check && outcome.is_ok() => {
                let serial = work.serial;
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(RECONNECT_WAIT).await;
                    let _ = this.update(cx, |this, cx| {
                        this.asked.retain(|work| {
                            work.serial != serial || !matches!(work.phase, Phase::Reconnecting(_))
                        });
                        this.mark();
                        cx.notify();
                    });
                })
                .detach();
                Phase::Reconnecting(link.clone())
            }
            _ => Phase::Answered,
        };
    }

    /// Writes what only this desktop knows onto each Machine: the work a person started here
    /// and when the Core's own download first failed. The adapter's updates carry neither.
    fn mark(&mut self) {
        for update in &mut self.machines {
            let machine = update.machine;
            let facts = update.facts.as_ref();
            let revision = facts.map(|facts| facts.revision);
            let own = self.tried.contains_key(&machine)
                || self.asked.iter().any(|work| work.machine == machine);
            let failure = facts
                .and_then(|facts| facts.last_failure.as_ref())
                .filter(|failure| failure.kind == FailureKind::Install(InstallFailure::Download));
            match (failure, self.retrying.get_mut(&machine)) {
                // A Machine out of reach reports nothing, which ends no failure.
                (None, _) if facts.is_none() => {}
                (None, _) => {
                    self.retrying.remove(&machine);
                }
                (Some(failure), None) => {
                    // The day counts on this desktop's clock, which the presentation reads.
                    let since = (!own && self.heard.contains(&machine)).then(crate::updates::now);
                    self.retrying.insert(
                        machine,
                        Retrying {
                            failure: failure.id,
                            since,
                        },
                    );
                }
                (Some(failure), Some(retrying)) => {
                    if own && retrying.failure != failure.id {
                        retrying.since = None;
                    }
                    retrying.failure = failure.id;
                }
            }
            if self
                .tried
                .get(&machine)
                .is_some_and(|answered| *answered != revision)
            {
                self.tried.remove(&machine);
            }
            let reachable = matches!(
                update.link,
                MachineLink::Connected | MachineLink::Incompatible(_)
            );
            if reachable && facts.is_some() {
                self.heard.insert(machine);
            } else {
                self.heard.remove(&machine);
            }
            let working = reachable && facts.is_some_and(|facts| facts.operation.is_some());
            self.asked.retain(|work| {
                work.machine != machine
                    || match &work.phase {
                        Phase::Requested => true,
                        Phase::Answered => working,
                        Phase::Reconnecting(link) => *link == update.link,
                    }
            });
            update.asked = self
                .asked
                .iter()
                .find(|work| work.machine == machine)
                .map(|work| work.asked);
            update.retrying_since = self
                .retrying
                .get(&machine)
                .and_then(|retrying| retrying.since);
        }
        let machines = &self.machines;
        let known = |machine: &MachineId| machines.iter().any(|update| update.machine == *machine);
        self.asked.retain(|work| known(&work.machine));
        self.retrying.retain(|machine, _| known(machine));
        self.tried.retain(|machine, _| known(machine));
        self.heard.retain(known);
    }

    /// Asks each affected Core to preview its Restart or Stop, all at once. Each preview that
    /// fails reports that failure to `surface` as its own action's outcome, as soon as it
    /// fails; the rest are prepared together. After a `stale` approval, a restart whose Pending
    /// Version is gone is dropped and one into another version asks.
    fn prepare(
        &self,
        actions: &[(MachineId, MachineAction)],
        always: bool,
        stale: Option<Approval>,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Task<Prepared> {
        let (answered, answers) = async_channel::unbounded();
        for (index, (machine, action)) in actions.iter().enumerate() {
            let answer = self.adapter.preview(*machine, action.clone());
            let answered = answered.clone();
            let (machine, action) = (*machine, action.clone());
            cx.background_executor()
                .spawn(async move {
                    let outcome = answer.recv().await.unwrap_or(Err(ActionFailure::Failed));
                    let _ = answered.send((index, machine, action, outcome)).await;
                })
                .detach();
        }
        drop(answered);
        cx.spawn(async move |this, cx| {
            let mut previews = Vec::new();
            while let Ok((index, machine, action, outcome)) = answers.recv().await {
                match outcome {
                    Ok(preview) => previews.push((index, preview)),
                    Err(failure) => {
                        let _ = this.update(cx, |this, cx| {
                            this.notify_outcome(machine, action, surface, Err(failure), cx);
                        });
                    }
                }
            }
            previews.sort_by_key(|(index, _)| *index);
            let mut previews: Vec<MachinePreview> =
                previews.into_iter().map(|(_, preview)| preview).collect();
            let mut always = always;
            if let Some(stale) = stale {
                previews.retain(|preview| stale.still_approves(preview));
                always |= previews.iter().any(|preview| stale.retargets(preview));
            }
            if previews.is_empty() {
                return Prepared::Nothing;
            }
            cx.update(
                |cx| match presentation::current(cx).previewed(&previews, always) {
                    Some(confirmation) => Prepared::Ask(confirmation, previews),
                    None => Prepared::Run(previews),
                },
            )
        })
    }

    /// Executes exactly `preview`, which the person confirmed on `surface`.
    pub(crate) fn execute(&mut self, preview: MachinePreview, surface: Surface) {
        self.performed
            .push((preview.machine, preview.action.clone(), surface));
        self.adapter.execute(preview);
    }

    fn notify_outcome(
        &self,
        machine: MachineId,
        action: MachineAction,
        surface: Surface,
        outcome: Result<(), ActionFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(update) = self.machines.iter().find(|known| known.machine == machine) else {
            return;
        };
        cx.emit(MachineUpdatesNotice {
            machine,
            name: update.name.clone(),
            local: update.local,
            action,
            surface,
            outcome,
        });
    }

    fn receive(&mut self, event: MachineUpdatesEvent, cx: &mut Context<Self>) {
        match event {
            MachineUpdatesEvent::Machines(machines) => {
                let previous = std::mem::replace(&mut self.machines, machines);
                for update in &mut self.machines {
                    update.failed_push = previous
                        .iter()
                        .find(|known| known.machine == update.machine)
                        .and_then(|known| carried_failure(known, update));
                }
            }
            MachineUpdatesEvent::Changed(update) => {
                let update = *update;
                match self
                    .machines
                    .iter_mut()
                    .find(|known| known.machine == update.machine)
                {
                    Some(known) => {
                        let finished = finished_update(known, &update);
                        let failed_push = carried_failure(known, &update);
                        *known = update;
                        known.failed_push = failed_push;
                        if let Some(text) = finished {
                            crate::updates::record_recent(text, cx);
                        }
                    }
                    None => self.machines.push(update),
                }
            }
            MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome,
            } => {
                let automatic = (action == MachineAction::Install)
                    .then(|| self.installing.iter().position(|(id, _)| *id == machine))
                    .flatten()
                    .map(|index| self.installing.remove(index).1);
                if action == MachineAction::Install {
                    self.record_push(machine, automatic.clone(), &outcome);
                }
                if automatic.is_none() {
                    self.answer(machine, &action, &outcome, cx);
                    self.finish_performed(machine, action, outcome, cx);
                }
            }
        }
        self.mark();
        self.install_bundled(cx);
        cx.notify();
    }

    /// Reports `outcome` to the surface that performed `action` on `machine`. An action no
    /// surface performed reports to none.
    fn finish_performed(
        &mut self,
        machine: MachineId,
        action: MachineAction,
        outcome: Result<(), ActionFailure>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self
            .performed
            .iter()
            .position(|(id, performed, _)| *id == machine && *performed == action)
        else {
            return;
        };
        let (_, _, surface) = self.performed.remove(index);
        if let MachineAction::DismissFailure(_) = action {
            return;
        }
        self.notify_outcome(machine, action, surface, outcome, cx);
    }

    /// Keeps the outcome of an Install that pushed the bundled Core: an automatic one of
    /// `automatic`, or one a surface performed while the Machine is offered a push. A finished
    /// Install clears the failure; an Install from the Core's feed records its own.
    fn record_push(
        &mut self,
        machine: MachineId,
        automatic: Option<Version>,
        outcome: &Result<(), ActionFailure>,
    ) {
        let Some(update) = self
            .machines
            .iter_mut()
            .find(|known| known.machine == machine)
        else {
            return;
        };
        match outcome {
            Ok(()) => update.failed_push = None,
            Err(failure) => {
                let Some(version) = automatic.or_else(|| update.push.clone()).or_else(|| {
                    matches!(failure, ActionFailure::NeedsFloor(_))
                        .then(|| update.bundled.clone())
                        .flatten()
                }) else {
                    return;
                };
                if update.push.as_ref() == Some(&version)
                    || update.bundled.as_ref() == Some(&version)
                {
                    update.failed_push = Some(FailedPush {
                        version,
                        failure: *failure,
                    });
                }
            }
        }
    }

    /// Installs the bundled Core on each Machine offered one that answers and runs no update
    /// operation, once per Machine and offered version.
    fn install_bundled(&mut self, cx: &App) {
        // Automatic pushes wait for the desktop's startup check, and a desktop that knows a newer
        // release of itself keeps waiting, so a Stepping Stone does not push its intermediate
        // Core to every Machine. A check that fails or ends without a result releases them.
        if let Some(service) = cx.try_global::<crate::updates::UpdateService>() {
            let updates = service.0.read(cx);
            if self.startup_check {
                if matches!(updates.state(), crate::updates::UpdateState::Checking) {
                    return;
                }
                self.startup_check = false;
            }
            if updates.pending_version().is_some() {
                return;
            }
        }
        let due: Vec<(MachineId, Version)> = self
            .machines
            .iter()
            .filter(|update| {
                matches!(
                    update.link,
                    MachineLink::Connected | MachineLink::Incompatible(_)
                ) && update
                    .facts
                    .as_ref()
                    .is_some_and(|facts| facts.operation.is_none())
            })
            .filter_map(|update| Some((update.machine, update.push.clone()?)))
            .collect();
        for (machine, version) in due {
            if self.offered.insert((machine, version.clone())) {
                self.installing.push((machine, version));
                self.adapter.perform(machine, MachineAction::Install);
            }
        }
    }
}

/// The failed push `previous` recorded, while `next` still offers its version.
fn carried_failure(previous: &MachineUpdate, next: &MachineUpdate) -> Option<FailedPush> {
    previous.failed_push.clone().filter(|failed| {
        next.push.as_ref() == Some(&failed.version)
            || next.bundled.as_ref() == Some(&failed.version)
    })
}

/// What Recent says when `next` finishes an update `previous` was waiting for.
fn finished_update(previous: &MachineUpdate, next: &MachineUpdate) -> Option<String> {
    let (before, after) = (previous.facts.as_ref()?, next.facts.as_ref()?);
    let name = &next.name;
    if before.running != after.running && is_newer(&after.running, &before.running) {
        return Some(format!("{name} updated to {}", after.running));
    }
    match (&before.pending, &after.pending) {
        (previous, Some(pending))
            if previous
                .as_ref()
                .is_none_or(|previous| previous.version != pending.version) =>
        {
            Some(format!("{name} installed {}", pending.version))
        }
        _ => None,
    }
}

fn is_newer(next: &Version, previous: &Version) -> bool {
    match (next.release(), previous.release()) {
        (Some(next), Some(previous)) => next > previous,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::super::testing::ScriptedMachines;
    use super::*;
    use crate::updates::presentation::Command;
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        FailureKind, InstallFailure, Operation, PendingVersion, Platform, Protocols, UpdateCheck,
        UpdateFacts, UpdateFailure,
    };

    #[test]
    fn replacing_a_pending_version_records_the_new_install() {
        assert_eq!(
            finished_update(
                &update("0.8.0", Some("0.9.0")),
                &update("0.8.0", Some("0.9.1"))
            ),
            Some("penguin installed 0.9.1".to_owned())
        );
        assert_eq!(
            finished_update(
                &update("0.8.0", Some("0.9.1")),
                &update("0.8.0", Some("0.9.1"))
            ),
            None
        );
    }

    fn update(running: &str, pending: Option<&str>) -> MachineUpdate {
        MachineUpdate {
            machine: [2; 32],
            name: "penguin".into(),
            local: false,
            link: super::super::MachineLink::Connected,
            facts: Some(UpdateFacts {
                revision: 1,
                platform: Platform::LinuxX86_64,
                running: Version::parse(running).unwrap(),
                pending: pending.map(|version| PendingVersion {
                    version: Version::parse(version).unwrap(),
                    protocols: Protocols::CURRENT,
                    breaking: false,
                    installed_at: 1,
                    overdue_since: None,
                    stranded: Vec::new(),
                }),
                sessions: 0,
                operation: None,
                check: UpdateCheck::Never,
                skipped: None,
                last_failure: None,
            }),
            capabilities: Capabilities::fake(&Capability::ALL),
            push: None,
            bundled: None,
            failed_push: None,
            asked: None,
            retrying_since: None,
        }
    }

    fn offered(update: MachineUpdate, push: &str) -> MachineUpdate {
        MachineUpdate {
            push: Some(Version::parse(push).unwrap()),
            ..update
        }
    }

    fn installed(cx: &mut gpui::TestAppContext) -> (Rc<ScriptedMachines>, Entity<MachineUpdates>) {
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
        (adapter, store)
    }

    /// A check or install a person starts is marked from the click until the Core's facts show
    /// the work ended, whichever of the request's answer and those facts arrives first.
    #[gpui::test]
    fn work_a_person_starts_is_marked_until_the_core_ends_it(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        let mut checking = penguin.clone();
        checking.facts.as_mut().unwrap().operation = Some(Operation::Checking);
        let asked = |cx: &mut gpui::TestAppContext| {
            store.read_with(cx, |store, _| store.machines()[0].asked)
        };
        let check_again = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::CheckAgain, Surface::Window, cx);
            });
        };
        let finished = |action| MachineUpdatesEvent::Finished {
            machine,
            action,
            outcome: Ok(()),
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin.clone()]));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        check_again(cx);
        assert_eq!(asked(cx), Some(Asked::Check), "from the click");
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(checking.clone())));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(Asked::Check));
        // The request answers before the facts that end the check arrive.
        adapter.emit(finished(MachineAction::CheckAgain));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(Asked::Check));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(penguin.clone())));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        // The facts that end the check arrive before the request answers.
        check_again(cx);
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(checking.clone())));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(penguin.clone())));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(Asked::Check));
        adapter.emit(finished(MachineAction::CheckAgain));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        store.update(cx, |store, cx| {
            store.perform(machine, MachineAction::Install, Surface::Settings, cx);
        });
        assert_eq!(asked(cx), Some(Asked::Install));
        adapter.emit(finished(MachineAction::Install));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        // The Core's own check is nobody's.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(checking)));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);
    }

    /// A request the Core refuses ran no work of the person's, so its mark ends with the answer
    /// and the Core's own operation stays nobody's.
    #[gpui::test]
    fn a_refused_request_marks_nothing(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        let mut checking = penguin.clone();
        checking.facts.as_mut().unwrap().operation = Some(Operation::Checking);
        let mut installing = penguin.clone();
        installing.facts.as_mut().unwrap().operation = Some(Operation::Installing {
            version: Version::parse("0.9.0").unwrap(),
            received: 1,
            total: 2,
        });
        adapter.emit(MachineUpdatesEvent::Machines(vec![checking]));
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.perform(machine, MachineAction::CheckAgain, Surface::Window, cx);
        });
        adapter.emit(MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::CheckAgain,
            outcome: Err(ActionFailure::Busy),
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(installing)));
        cx.run_until_parked();
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            None
        );

        // The Core's own download then fails. That failure is not the person's either.
        let mut failed = penguin;
        let facts = failed.facts.as_mut().unwrap();
        facts.revision = 2;
        facts.last_failure = Some(UpdateFailure {
            id: 1,
            at: 500,
            version: Version::parse("0.9.0").unwrap(),
            kind: FailureKind::Install(InstallFailure::Download),
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(failed)));
        cx.run_until_parked();
        assert!(
            store
                .read_with(cx, |store, _| store.machines()[0].retrying_since)
                .is_some()
        );
    }

    /// An install that ran and failed keeps its mark until the Core's facts end the operation,
    /// and its failure is the person's own.
    #[gpui::test]
    fn an_install_that_ran_and_failed_keeps_its_mark_until_the_facts_end_it(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        let mut installing = penguin.clone();
        installing.facts.as_mut().unwrap().operation = Some(Operation::Installing {
            version: Version::parse("0.9.0").unwrap(),
            received: 1,
            total: 2,
        });
        let mut failed = penguin.clone();
        let facts = failed.facts.as_mut().unwrap();
        facts.revision = 2;
        facts.last_failure = Some(UpdateFailure {
            id: 1,
            at: 500,
            version: Version::parse("0.9.0").unwrap(),
            kind: FailureKind::Install(InstallFailure::Download),
        });
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin]));
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.perform(machine, MachineAction::Install, Surface::Window, cx);
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(installing)));
        adapter.emit(MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::Install,
            outcome: Err(ActionFailure::Failed),
        });
        cx.run_until_parked();
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            Some(Asked::Install)
        );
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(failed)));
        cx.run_until_parked();
        let (asked, since) = store.read_with(cx, |store, _| {
            let update = &store.machines()[0];
            (update.asked, update.retrying_since)
        });
        assert_eq!((asked, since), (None, None));
    }

    /// A mark ends with its Machine: when the Machine goes out of reach after the request
    /// answered, and when it is removed.
    #[gpui::test]
    fn a_mark_ends_when_its_machine_goes_out_of_reach_or_is_removed(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        let mut installing = penguin.clone();
        installing.facts.as_mut().unwrap().operation = Some(Operation::Installing {
            version: Version::parse("0.9.0").unwrap(),
            received: 1,
            total: 2,
        });
        let asked = |cx: &mut gpui::TestAppContext| {
            store.read_with(cx, |store, _| {
                store.machines().first().and_then(|update| update.asked)
            })
        };
        let install = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::Install, Surface::Window, cx);
            });
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin.clone()]));
        cx.run_until_parked();

        install(cx);
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(installing.clone())));
        adapter.emit(MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::Install,
            outcome: Ok(()),
        });
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(Asked::Install));
        // The Machine keeps its last facts while no route reaches it.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(MachineUpdate {
            link: super::super::MachineLink::Offline { since: 1 },
            ..installing
        })));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        adapter.emit(MachineUpdatesEvent::Changed(Box::new(penguin.clone())));
        cx.run_until_parked();
        install(cx);
        assert_eq!(asked(cx), Some(Asked::Install));
        adapter.emit(MachineUpdatesEvent::Machines(Vec::new()));
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin]));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);
    }

    /// Check Again on This Mac or This Computer has no route to try again, so no wait follows
    /// its answer.
    #[gpui::test]
    fn check_again_on_the_local_machine_waits_for_no_reconnect(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let local = MachineUpdate {
            local: true,
            link: super::super::MachineLink::Unconfirmed {
                expected: Version::parse("0.9.0").unwrap(),
                since: 1,
            },
            ..update("0.8.0", None)
        };
        let machine = local.machine;
        adapter.emit(MachineUpdatesEvent::Machines(vec![local]));
        cx.run_until_parked();
        store.update(cx, |store, cx| {
            store.perform(machine, MachineAction::CheckAgain, Surface::Window, cx);
        });
        adapter.emit(MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::CheckAgain,
            outcome: Ok(()),
        });
        cx.run_until_parked();
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            None
        );
    }

    /// The bundled Core's automatic install is not a person's, so it is not marked.
    #[gpui::test]
    fn an_automatic_install_is_not_marked(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = offered(update("0.8.0", None), "0.9.0");
        let machine = penguin.machine;
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin.clone()]));
        cx.run_until_parked();
        assert_eq!(adapter.performed(), [(machine, MachineAction::Install)]);
        let mut installing = penguin;
        installing.facts.as_mut().unwrap().operation = Some(Operation::Installing {
            version: Version::parse("0.9.0").unwrap(),
            received: 1,
            total: 2,
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(installing)));
        cx.run_until_parked();
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            None
        );
    }

    /// A Core's own failed download that begins in this run is dated from its first failure
    /// while the Core keeps trying, until a person's own try from this desktop fails.
    #[gpui::test]
    fn a_cores_own_failed_download_is_dated_until_a_persons_try_fails(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, store) = installed(cx);
        let failed = |id, at, reason| {
            let mut update = update("0.8.0", None);
            let facts = update.facts.as_mut().unwrap();
            facts.revision = id + 1;
            facts.last_failure = Some(UpdateFailure {
                id,
                at,
                version: Version::parse("0.9.0").unwrap(),
                kind: FailureKind::Install(reason),
            });
            update
        };
        let machine = update("0.8.0", None).machine;
        let since = |cx: &mut gpui::TestAppContext| {
            store.read_with(cx, |store, _| store.machines()[0].retrying_since)
        };
        let changed = |update, cx: &mut gpui::TestAppContext| {
            adapter.emit(MachineUpdatesEvent::Changed(Box::new(update)));
            cx.run_until_parked();
        };
        let try_again = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::Install, Surface::Window, cx);
            });
        };
        let answer = |cx: &mut gpui::TestAppContext| {
            adapter.emit(MachineUpdatesEvent::Finished {
                machine,
                action: MachineAction::Install,
                outcome: Err(ActionFailure::Failed),
            });
            cx.run_until_parked();
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![update("0.8.0", None)]));
        cx.run_until_parked();
        // The Machine's clock is far behind. The day counts on this desktop's clock.
        let began = crate::updates::now();
        changed(failed(1, 500, InstallFailure::Download), cx);
        let first = since(cx);
        assert!(first.is_some_and(|since| since >= began));
        // The Core tries again and fails again.
        changed(failed(2, 900, InstallFailure::Download), cx);
        assert_eq!(since(cx), first);
        // A Machine out of reach for a moment keeps the day it began.
        changed(
            MachineUpdate {
                link: super::super::MachineLink::Offline { since: 1 },
                facts: None,
                ..update("0.8.0", None)
            },
            cx,
        );
        changed(failed(2, 900, InstallFailure::Download), cx);
        assert_eq!(since(cx), first);
        // Only a download is tried again by itself.
        changed(failed(3, 950, InstallFailure::DiskFull), cx);
        assert_eq!(since(cx), None);
        changed(failed(4, 2_000, InstallFailure::Download), cx);
        let first = since(cx);
        assert!(first.is_some());

        // The person's try ends the quiet only once it fails, whether its answer or the facts
        // that record the failure arrive first.
        try_again(cx);
        assert_eq!(since(cx), first, "the old failure stays quiet");
        answer(cx);
        assert_eq!(since(cx), first);
        changed(failed(5, 2_100, InstallFailure::Download), cx);
        assert_eq!(since(cx), None, "the person's own try failed");
        // The Core's later tries change nothing while that failure stands.
        changed(failed(6, 2_500, InstallFailure::Download), cx);
        assert_eq!(since(cx), None);

        // A download that works ends it, and a later failure is dated anew.
        changed(update("0.8.0", None), cx);
        changed(failed(7, 3_000, InstallFailure::Download), cx);
        assert!(since(cx).is_some());
        try_again(cx);
        changed(failed(8, 3_100, InstallFailure::Download), cx);
        assert_eq!(since(cx), None, "the person's own try failed");
        answer(cx);
        assert_eq!(since(cx), None);
    }

    /// A failure a Machine already reports when this run first hears from it, or when it comes
    /// back into reach, has no known first time, so it is not dated and shows at once.
    #[gpui::test]
    fn a_failed_download_that_began_unseen_is_not_dated(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let mut failed = update("0.8.0", None);
        failed.facts.as_mut().unwrap().last_failure = Some(UpdateFailure {
            id: 1,
            at: 500,
            version: Version::parse("0.9.0").unwrap(),
            kind: FailureKind::Install(InstallFailure::Download),
        });
        let since = |cx: &mut gpui::TestAppContext| {
            store.read_with(cx, |store, _| store.machines()[0].retrying_since)
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![failed.clone()]));
        cx.run_until_parked();
        assert_eq!(since(cx), None, "older than this run");

        for update in [
            update("0.8.0", None),
            MachineUpdate {
                link: super::super::MachineLink::Offline { since: 1 },
                ..update("0.8.0", None)
            },
            failed,
        ] {
            adapter.emit(MachineUpdatesEvent::Changed(Box::new(update)));
            cx.run_until_parked();
        }
        assert_eq!(since(cx), None, "it failed while out of reach");
    }

    /// Check Again on a Machine no route reaches answers at once while its routes try again.
    /// The mark stays until the Machine's link changes, or the wait for that ends.
    #[gpui::test]
    fn check_again_stays_marked_while_an_unreachable_machine_reconnects(
        cx: &mut gpui::TestAppContext,
    ) {
        let (adapter, store) = installed(cx);
        let unconfirmed = MachineUpdate {
            link: super::super::MachineLink::Unconfirmed {
                expected: Version::parse("0.9.0").unwrap(),
                since: 1,
            },
            ..update("0.8.0", None)
        };
        let machine = unconfirmed.machine;
        let asked = |cx: &mut gpui::TestAppContext| {
            store.read_with(cx, |store, _| store.machines()[0].asked)
        };
        let check_again = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::CheckAgain, Surface::Window, cx);
            });
            adapter.emit(MachineUpdatesEvent::Finished {
                machine,
                action: MachineAction::CheckAgain,
                outcome: Ok(()),
            });
            cx.run_until_parked();
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![unconfirmed.clone()]));
        cx.run_until_parked();

        check_again(cx);
        assert_eq!(asked(cx), Some(Asked::Check));
        // A route answers.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update(
            "0.9.0", None,
        ))));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        // No route answers.
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(unconfirmed)));
        cx.run_until_parked();
        check_again(cx);
        cx.executor()
            .advance_clock(RECONNECT_WAIT - std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(Asked::Check));
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);
    }

    /// The bundled Core installs with no click on each Machine offered it, once per Machine and
    /// version, only while the Machine answers and runs no update operation, and a failure
    /// reaches no surface and is not retried.
    #[gpui::test]
    fn the_bundled_core_installs_once_per_machine_and_version(cx: &mut gpui::TestAppContext) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        updater.emit(crate::updates::UpdateEvent::UpToDate);
        cx.run_until_parked();
        let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let notices = notices.clone();
            cx.subscribe(&store, move |_, notice: &MachineUpdatesNotice, _| {
                notices.borrow_mut().push(notice.clone());
            })
        });
        let local = MachineUpdate {
            machine: [1; 32],
            name: "This Mac".into(),
            local: true,
            ..update("0.8.0", None)
        };
        let offline = MachineUpdate {
            link: super::super::MachineLink::Offline { since: 1 },
            ..offered(update("0.8.0", None), "0.9.0")
        };
        let mut checking = offered(local.clone(), "0.9.0");
        checking.facts.as_mut().unwrap().operation = Some(Operation::Checking);
        adapter.emit(MachineUpdatesEvent::Machines(vec![checking, offline]));
        cx.run_until_parked();
        assert!(
            adapter.performed().is_empty(),
            "busy or offline Machines wait"
        );

        adapter.emit(MachineUpdatesEvent::Changed(Box::new(offered(
            local.clone(),
            "0.9.0",
        ))));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(offered(
            update("0.8.0", None),
            "0.9.0",
        ))));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(offered(
            local.clone(),
            "0.9.0",
        ))));
        cx.run_until_parked();
        let install = MachineAction::Install;
        assert_eq!(
            adapter.performed(),
            [([1; 32], install.clone()), ([2; 32], install.clone())]
        );

        adapter.emit(MachineUpdatesEvent::Finished {
            machine: [2; 32],
            action: install.clone(),
            outcome: Err(ActionFailure::Failed),
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(offered(
            update("0.8.0", None),
            "0.9.0",
        ))));
        cx.run_until_parked();
        assert!(
            notices.borrow().is_empty(),
            "an automatic install has no surface"
        );
        assert_eq!(
            adapter.performed().len(),
            2,
            "a failed install is not retried"
        );

        adapter.emit(MachineUpdatesEvent::Changed(Box::new(offered(
            update("0.8.0", None),
            "0.9.1",
        ))));
        cx.run_until_parked();
        assert_eq!(adapter.performed()[2], ([2; 32], install));
    }

    /// A push that breaks off or runs out of disk space records nothing in the Core's facts.
    /// The failed automatic install still shows on every surface with Try Again, which installs
    /// again, and nothing retries it automatically.
    #[gpui::test]
    fn a_failed_automatic_install_offers_try_again_without_retrying(cx: &mut gpui::TestAppContext) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        updater.emit(crate::updates::UpdateEvent::UpToDate);
        cx.run_until_parked();
        let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
        let penguin = [2; 32];
        let install = MachineAction::Install;
        let idle = || offered(update("0.8.0", None), "0.9.0");
        adapter.emit(MachineUpdatesEvent::Machines(vec![idle()]));
        cx.run_until_parked();
        assert_eq!(adapter.performed(), [(penguin, install.clone())]);

        for (failure, sentence) in [
            (
                ActionFailure::Unreachable,
                "SpaceTerm Core 0.9.0 couldn't install on penguin because penguin stopped \
                 answering. penguin still runs SpaceTerm Core 0.8.0, and its Terminal Sessions \
                 keep running. Choose Try Again to install it.",
            ),
            (
                ActionFailure::DiskFull,
                "SpaceTerm Core 0.9.0 couldn't install on penguin because penguin is out of disk \
                 space. penguin still runs SpaceTerm Core 0.8.0, and its Terminal Sessions keep \
                 running. Free some space, then choose Try Again.",
            ),
        ] {
            adapter.emit(MachineUpdatesEvent::Finished {
                machine: penguin,
                action: install.clone(),
                outcome: Err(failure),
            });
            adapter.emit(MachineUpdatesEvent::Changed(Box::new(idle())));
            cx.run_until_parked();
            let shown = cx.update(|cx| presentation::current(cx));
            let view = shown.machine(penguin).unwrap();
            assert_eq!(view.control_label(), "penguin Install Failed");
            assert_eq!(view.sentence().as_ref(), sentence);
            let row = view.settings_row();
            assert_eq!(row.status.as_ref(), "Install failed");
            let button = row.button.expect("Settings offers Try Again");
            assert_eq!(button.label.as_ref(), "Try Again");
            assert_eq!(button.command, Command::Machine(penguin, install.clone()));
            let items = view.extra_row().items.unwrap();
            assert!(items.iter().any(|item| matches!(
                item,
                presentation::ExtraItem::Action { title, command, .. }
                    if title.as_ref() == "Try Again"
                        && *command == Command::Machine(penguin, install.clone())
            )));
            assert!(
                shown
                    .items()
                    .iter()
                    .any(|item| item.command == Some(Command::Machine(penguin, install.clone()))),
                "the title bar offers it too"
            );
            assert_eq!(
                adapter.performed().len(),
                if failure == ActionFailure::Unreachable {
                    1
                } else {
                    2
                },
                "nothing retries a failed install automatically"
            );

            // Try Again installs again.
            cx.update(|cx| {
                store.update(cx, |store, cx| {
                    store.perform(penguin, install.clone(), Surface::Settings, cx);
                });
            });
        }
        assert_eq!(adapter.performed().len(), 3);

        // A finished install clears the failure.
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: penguin,
            action: install.clone(),
            outcome: Ok(()),
        });
        cx.run_until_parked();
        let shown = cx.update(|cx| presentation::current(cx));
        assert_eq!(
            shown.machine(penguin).unwrap().state,
            presentation::MachineState::Available {
                to: Version::parse("0.9.0").unwrap()
            }
        );
    }

    /// A Core too old for this desktop's work major keeps its failed automatic install on every
    /// surface, with the remedy, the incompatibility and Try Again, instead of a generic Update.
    #[gpui::test]
    fn an_incompatible_core_shows_its_failed_automatic_install(cx: &mut gpui::TestAppContext) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        updater.emit(crate::updates::UpdateEvent::UpToDate);
        cx.run_until_parked();
        let penguin = [2; 32];
        let install = MachineAction::Install;
        let incompatible = || MachineUpdate {
            link: super::super::MachineLink::Incompatible(
                spaceterm_core::protocol::version::Incompatible::ListenerTooOld,
            ),
            ..offered(update("0.8.0", None), "0.9.0")
        };
        adapter.emit(MachineUpdatesEvent::Machines(vec![incompatible()]));
        cx.run_until_parked();
        assert_eq!(adapter.performed(), [(penguin, install.clone())]);

        adapter.emit(MachineUpdatesEvent::Finished {
            machine: penguin,
            action: install.clone(),
            outcome: Err(ActionFailure::DiskFull),
        });
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(incompatible())));
        cx.run_until_parked();
        let shown = cx.update(|cx| presentation::current(cx));
        let view = shown.machine(penguin).unwrap();
        assert_eq!(view.control_label(), "penguin Install Failed");
        assert_eq!(
            view.sentence().as_ref(),
            "SpaceTerm Core 0.9.0 couldn't install on penguin because penguin is out of disk \
             space. penguin still runs SpaceTerm Core 0.8.0, which is too old for this \
             SpaceTerm. Its Terminal Sessions keep running. Free some space, then choose Try \
             Again."
        );
        let row = view.settings_row();
        assert_eq!(row.status.as_ref(), "Install failed");
        let button = row.button.expect("Settings offers Try Again");
        assert_eq!(button.label.as_ref(), "Try Again");
        assert_eq!(button.command, Command::Machine(penguin, install.clone()));
        let items = view.extra_row().items.unwrap();
        assert!(items.iter().any(|item| matches!(
            item,
            presentation::ExtraItem::Action { title, command, .. }
                if title.as_ref() == "Try Again"
                    && *command == Command::Machine(penguin, install.clone())
        )));
        assert_eq!(adapter.performed().len(), 1, "nothing retries it");
    }

    #[gpui::test]
    fn recent_names_each_install_and_update_and_failures_reach_observers(
        cx: &mut gpui::TestAppContext,
    ) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        updater.emit(crate::updates::UpdateEvent::UpToDate);
        cx.run_until_parked();
        let store = cx.update(|cx| MachineUpdates::global(cx).unwrap());
        let notices = Rc::new(std::cell::RefCell::new(Vec::new()));
        let _subscription = cx.update(|cx| {
            let notices = notices.clone();
            cx.subscribe(&store, move |_, notice: &MachineUpdatesNotice, _| {
                notices.borrow_mut().push(notice.clone());
            })
        });

        adapter.emit(MachineUpdatesEvent::Machines(vec![update("0.8.0", None)]));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update(
            "0.8.0",
            Some("0.9.0"),
        ))));
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update(
            "0.9.0", None,
        ))));
        // An action no surface performed reports to none.
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: [2; 32],
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Busy),
        });
        cx.run_until_parked();
        assert!(notices.borrow().is_empty());
        cx.update(|cx| {
            store.update(cx, |store, cx| {
                store.perform([2; 32], MachineAction::Restart, Surface::StatusMenu, cx);
            });
        });
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: [2; 32],
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Busy),
        });
        cx.run_until_parked();

        let recent: Vec<String> = cx.read(|cx| {
            cx.global::<crate::updates::UpdateService>()
                .0
                .read(cx)
                .recent()
                .iter()
                .map(|entry| entry.text.clone())
                .collect()
        });
        assert_eq!(
            recent,
            ["penguin updated to 0.9.0", "penguin installed 0.9.0"]
        );
        assert_eq!(notices.borrow().len(), 1);
        assert_eq!(notices.borrow()[0].surface, Surface::StatusMenu);
        assert_eq!(notices.borrow()[0].outcome, Err(ActionFailure::Busy));

        cx.update(|cx| {
            store.update(cx, |store, cx| {
                store.perform(
                    [2; 32],
                    MachineAction::DismissFailure(4),
                    Surface::Window,
                    cx,
                );
            });
        });
        assert!(cx.read(|cx| store.read(cx).dismissed().contains(&([2; 32], 4))));
        assert_eq!(
            adapter.performed(),
            vec![
                ([2; 32], MachineAction::Restart),
                ([2; 32], MachineAction::DismissFailure(4)),
            ]
        );
    }
    #[gpui::test]
    fn a_desktop_with_a_newer_release_waiting_does_not_push_its_core(
        cx: &mut gpui::TestAppContext,
    ) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        adapter.emit(MachineUpdatesEvent::Machines(vec![offered(
            update("0.8.0", None),
            "0.9.0",
        )]));
        cx.run_until_parked();
        assert!(
            adapter.performed().is_empty(),
            "wait for the desktop's startup check"
        );
        updater.emit(crate::updates::UpdateEvent::Available("0.10.0".into()));
        cx.run_until_parked();
        assert!(
            adapter.performed().is_empty(),
            "the intermediate desktop must not push its Core"
        );
    }

    #[gpui::test]
    fn a_desktop_whose_check_fails_still_pushes_its_core(cx: &mut gpui::TestAppContext) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater.clone(), cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
        adapter.emit(MachineUpdatesEvent::Machines(vec![offered(
            update("0.8.0", None),
            "0.9.0",
        )]));
        cx.run_until_parked();
        assert!(adapter.performed().is_empty());
        updater.emit(crate::updates::UpdateEvent::Failed(
            crate::updates::UpdateError::Check,
        ));
        cx.run_until_parked();
        assert_eq!(adapter.performed(), [([2; 32], MachineAction::Install)]);
    }
}
