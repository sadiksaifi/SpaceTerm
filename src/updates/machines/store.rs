//! The application's one view of Machine updates: it starts the composed
//! [`MachineUpdatesAdapter`], keeps the latest [`MachineUpdate`] per Machine and the failure
//! failure steps dismissed on this desktop and the restart confirmations cancelled here, and
//! performs actions. Observers of [`MachineUpdatesService`] hear every change. [`MachineUpdatesNotice`]
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
//! It also marks each Machine with what only this desktop knows: the check, install or restart a
//! person started here that the Core has not ended. The update view shows that work, and keeps
//! quiet about the work a Core starts by itself.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use spaceterm_core::update::facts::{Operation, UpdateCheck, Version};

use super::{
    ActionFailure, Asked, FailedPush, MachineAction, MachineId, MachineLink, MachinePreview,
    MachineUpdate, MachineUpdatesAdapter, MachineUpdatesEvent,
};
use crate::updates::confirm::{self, Approval, Confirmation};
use crate::updates::state::{Checked, Dismissed, PUSH_FAILURE};

/// How long Check Again reads as Checking on a Machine no route reached, while its routes try
/// again.
pub(crate) const RECONNECT_WAIT: Duration = Duration::from_secs(10);

/// The adapter's restart deadline. Past it, a restart that never came back reads as
/// Unconfirmed, and a restart approved here stays this desktop's step as Restart Not Confirmed.
pub(crate) const RESTART_DEADLINE: Duration = super::live::RESTART_LIMIT;

/// A check, install or restart a person started, until the Core's facts show it ended.
struct AskedWork {
    machine: MachineId,
    asked: Asked,
    phase: Phase,
    /// Names this work to the timer that ends its reconnect or restart wait.
    serial: u64,
    /// A restart: the facts showed it begin, by a restart operation or the Core going away.
    began: bool,
    /// A restart: the running version the approved preview observed, so a Core that came back
    /// on another version settles the restart even when its restart was never observed.
    running: Option<Version>,
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
    /// The Core refused the click's restart at once, while the person waits on it: at its
    /// preview, or when the approved restart executed. Only such a refusal opens an alert unasked
    /// (D5); a later outcome shows once in the title bar.
    pub(crate) refused_at_once: bool,
}

impl MachineUpdatesNotice {
    /// The alert this outcome opens without a click on it, as a title and a message: D5 for an
    /// action the Core refused at its preview, and a failed Start or Stop, which no update step
    /// reports. A later outcome of an update action shows once in the title bar instead (design
    /// 4.6). `Changed` and `LoginItemOff` have no alert.
    pub(crate) fn unasked_alert(&self) -> Option<(SharedString, SharedString)> {
        let Err(failure) = self.outcome else {
            return None;
        };
        let waiting = self.refused_at_once
            || matches!(self.action, MachineAction::Start | MachineAction::Stop);
        if !waiting {
            return None;
        }
        crate::updates::copy::dialogs::action_failure(&self.name, &self.action, failure)
    }
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
                        defer(&actions, cx);
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
        store.update(cx, |store, cx| {
            for preview in previews {
                store.execute(preview, surface, cx);
            }
        });
    }
}

/// The person cancelled a restart confirmation: at the Ask stage it hides that Machine's step
/// for [`crate::updates::ladder::ASK_DEFERRAL`] on this desktop, in this run.
fn defer(actions: &[(MachineId, MachineAction)], cx: &mut App) {
    let Some(store) = MachineUpdates::global(cx) else {
        return;
    };
    let now = crate::updates::now();
    store.update(cx, |store, cx| {
        for (machine, action) in actions {
            if *action == MachineAction::Restart {
                store.defer(*machine, now, cx);
            }
        }
    });
}

pub(crate) struct MachineUpdates {
    adapter: Rc<dyn MachineUpdatesAdapter>,
    machines: Vec<MachineUpdate>,
    dismissed: Dismissed,
    /// Actions performed that have not finished, oldest first.
    performed: Vec<(MachineId, MachineAction, Surface)>,
    /// Each check or install a person started that has not ended, oldest first.
    asked: Vec<AskedWork>,
    /// How many checks, installs and restarts people started, which names the next one.
    serial: u64,
    /// When the person last cancelled each Machine's restart confirmation.
    deferred: HashMap<MachineId, u64>,
    /// Machines where a person's check or install answered, with the facts revision then. The
    /// next revision is that work's result, so a failure it records is the person's own.
    tried: HashMap<MachineId, Option<u64>>,
    /// Machines in reach whose facts this run has received. A failure in the first facts of a
    /// Machine, or in its first after it was out of reach, began unseen.
    heard: HashSet<MachineId>,
    /// How the last check a person started on each Machine ended, in this run.
    checked: HashMap<MachineId, Checked>,
    /// Machines whose check answered, with the facts revision then. The Core's facts may
    /// arrive after the answer, and the first newer revision settles the check's outcome.
    unsettled: HashMap<MachineId, Option<u64>>,
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
                deferred: HashMap::new(),
                tried: HashMap::new(),
                checked: HashMap::new(),
                unsettled: HashMap::new(),
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

    /// When the person last cancelled each Machine's restart confirmation, in this run.
    pub(crate) fn deferred(&self) -> &HashMap<MachineId, u64> {
        &self.deferred
    }

    /// How the last check a person started on each Machine ended, in this run.
    pub(crate) fn checked(&self) -> &HashMap<MachineId, Checked> {
        &self.checked
    }

    /// Records that the person cancelled `machine`'s restart confirmation at `at`, when its
    /// Pending Version is at the Ask stage and is no hard stop. The update view then hides that
    /// Machine's step for a day on this desktop. Cancel at Offer or Insist records nothing, so
    /// it never hides a later stage's first step.
    pub(crate) fn defer(&mut self, machine: MachineId, at: u64, cx: &mut Context<Self>) {
        if !self.cancel_defers(machine, at, cx) {
            return;
        }
        self.deferred.insert(machine, at);
        cx.notify();
    }

    /// Whether Cancel in `machine`'s restart confirmation at `now` hides its step for a day.
    pub(crate) fn cancel_defers(&self, machine: MachineId, now: u64, cx: &App) -> bool {
        let Some(update) = self.machines.iter().find(|known| known.machine == machine) else {
            return false;
        };
        let marks = crate::updates::state::Marks {
            dismissed: &self.dismissed,
            deferred: &HashMap::new(),
            pushes_held: self.holds_pushes(cx),
            checked: &self.checked,
        };
        crate::updates::state::cancel_defers(update, marks, now)
    }

    /// Whether automatic pushes wait for SpaceTerm's startup check or a newer SpaceTerm.
    pub(crate) fn holds_pushes(&self, cx: &App) -> bool {
        cx.try_global::<crate::updates::UpdateService>()
            .is_some_and(|service| {
                let updates = service.0.read(cx);
                (self.startup_check
                    && matches!(updates.state(), crate::updates::UpdateState::Checking))
                    || updates.pending_version().is_some()
            })
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
                // A failed push and a skipped Stepping Stone have no failure the Core retires.
                if id >= crate::updates::state::STEPPING_STONE {
                    self.mark();
                    cx.notify();
                    return;
                }
                None
            }
            MachineAction::CheckAgain | MachineAction::ConnectAndCheck => Some(Asked::Check),
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
                began: false,
                running: None,
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
            MachineAction::CheckAgain | MachineAction::ConnectAndCheck => Asked::Check,
            MachineAction::Install => Asked::Install,
            MachineAction::Restart => Asked::Restart,
            _ => return,
        };
        // Connect and Check answers only once it connected and the check ran, or failed to.
        let connected_here = *action == MachineAction::ConnectAndCheck;
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
        // A restart that loses its lane answers Unreachable although the Core may have accepted
        // it, so only the facts settle that restart.
        let unknown = asked == Asked::Restart && *outcome == Err(ActionFailure::Unreachable);
        if !unknown
            && matches!(
                outcome,
                Err(ActionFailure::Busy
                    | ActionFailure::Unavailable(_)
                    | ActionFailure::Unreachable
                    | ActionFailure::RelaunchRequired
                    | ActionFailure::Changed
                    | ActionFailure::LoginItemOff)
            )
        {
            self.asked.remove(index);
            // A check of a Machine in reach that never ran, or failed before its facts said so,
            // is the person's failure to read, bound to the check those facts carried then.
            if asked == Asked::Check
                && let Some(update) = update.filter(|update| {
                    matches!(
                        update.link,
                        MachineLink::Connected | MachineLink::Incompatible(_)
                    )
                })
            {
                let observed = update.facts.as_ref().map(|facts| facts.check.clone());
                self.checked.insert(machine, Checked::Refused(observed));
                self.unsettled.remove(&machine);
            }
            return;
        }
        self.tried.insert(machine, revision);
        // This Mac or This Computer has no route to try again, and Connect and Check already
        // waited for one.
        let unreachable = update
            .filter(|update| !update.local && !connected_here)
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
        if asked == Asked::Check {
            self.settle_check(machine, revision, connected_here);
        }
    }

    /// Records the outcome of a check a person asked for on `machine`, which answered at facts
    /// `revision`, from the facts the store holds now. A Machine out of reach ran no check: its
    /// routes try again, and its row says so.
    /// `connected` says Connect and Check reached the Core, whose link and facts the store may
    /// not have heard yet; its newer facts then settle the outcome.
    fn settle_check(&mut self, machine: MachineId, revision: Option<u64>, connected: bool) {
        let Some(update) = self.machines.iter().find(|known| known.machine == machine) else {
            return;
        };
        let reachable = matches!(
            update.link,
            MachineLink::Connected | MachineLink::Incompatible(_)
        );
        if connected && !reachable {
            self.checked.remove(&machine);
            self.unsettled.insert(machine, revision);
            return;
        }
        match update.facts.as_ref().filter(|_| reachable) {
            Some(facts) => {
                self.checked
                    .insert(machine, Checked::Finished(facts.check.clone()));
                self.unsettled.insert(machine, revision);
            }
            None => {
                self.checked.remove(&machine);
                self.unsettled.remove(&machine);
            }
        }
    }

    /// Writes what only this desktop knows onto each Machine: the work a person started here.
    /// The adapter's updates carry none.
    fn mark(&mut self) {
        for update in &mut self.machines {
            let machine = update.machine;
            let facts = update.facts.as_ref();
            let revision = facts.map(|facts| facts.revision);
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
            let restarting = reachable
                && facts.is_some_and(|facts| {
                    matches!(
                        facts.operation,
                        Some(Operation::Restarting { .. } | Operation::RestartingSameVersion)
                    )
                });
            let running = facts.map(|facts| &facts.running);
            // Newer facts after a check answered carry its outcome once their check differs
            // from the one recorded at the answer. Facts that came before the answer already
            // carry it, as recorded there, and a revision that changes no check settles nothing.
            if let Some(answered) = self.unsettled.get(&machine)
                && *answered != revision
                && let Some(facts) = facts
                && !matches!(
                    self.checked.get(&machine),
                    Some(Checked::Finished(recorded)) if *recorded == facts.check
                )
            {
                self.checked
                    .insert(machine, Checked::Finished(facts.check.clone()));
                self.unsettled.remove(&machine);
            }
            // A newer completed check retires a refused one: a success clears it, and a failure
            // stays as that check's outcome.
            if let Some(Checked::Refused(observed)) = self.checked.get(&machine)
                && let Some(facts) = facts
                && Some(&facts.check) != observed.as_ref()
            {
                if matches!(facts.check, UpdateCheck::Failing { .. }) {
                    self.checked
                        .insert(machine, Checked::Finished(facts.check.clone()));
                } else {
                    self.checked.remove(&machine);
                }
            }
            self.asked.retain_mut(|work| {
                if work.machine != machine {
                    return true;
                }
                match (&work.phase, work.asked) {
                    // A restart stays marked from the approval, whether or not its request has
                    // answered, until the Core comes back without one: after it was seen to
                    // begin, or on another version than the approval observed. While its
                    // Machine does not answer it stays marked, so past the adapter's deadline
                    // the Unconfirmed link is this desktop's step.
                    (_, Asked::Restart) => {
                        if restarting || !reachable {
                            work.began = true;
                            return true;
                        }
                        let moved = work
                            .running
                            .as_ref()
                            .zip(running)
                            .is_some_and(|(approved, now)| approved != now);
                        !(work.began || moved)
                    }
                    (Phase::Requested, _) => true,
                    (Phase::Answered, _) => working,
                    (Phase::Reconnecting(link), _) => *link == update.link,
                }
            });
            update.asked = self
                .asked
                .iter()
                .find(|work| work.machine == machine)
                .map(|work| work.asked);
        }
        let machines = &self.machines;
        let known = |machine: &MachineId| machines.iter().any(|update| update.machine == *machine);
        self.asked.retain(|work| known(&work.machine));
        self.tried.retain(|machine, _| known(machine));
        self.checked.retain(|machine, _| known(machine));
        self.unsettled.retain(|machine, _| known(machine));
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
                            this.notify_outcome(machine, action, surface, Err(failure), true, cx);
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
            cx.update(|cx| {
                let store = MachineUpdates::global(cx);
                let machines = store
                    .as_ref()
                    .map(|store| store.read(cx).machines.clone())
                    .unwrap_or_default();
                let now = crate::updates::now();
                match confirm::previewed(&machines, &previews, always) {
                    Some(confirmation) => Prepared::Ask(
                        confirmation.defers(|machine| {
                            store
                                .as_ref()
                                .is_some_and(|store| store.read(cx).cancel_defers(machine, now, cx))
                        }),
                        previews,
                    ),
                    None => Prepared::Run(previews),
                }
            })
        })
    }

    /// Executes exactly `preview`, which the person confirmed on `surface`. A restart stays
    /// marked as this desktop's until the Core comes back, or for the restart deadline while
    /// the facts never show it begin.
    pub(crate) fn execute(
        &mut self,
        preview: MachinePreview,
        surface: Surface,
        cx: &mut Context<Self>,
    ) {
        if preview.action == MachineAction::Restart {
            self.serial += 1;
            let serial = self.serial;
            self.asked.retain(|work| work.machine != preview.machine);
            self.asked.push(AskedWork {
                machine: preview.machine,
                asked: Asked::Restart,
                phase: Phase::Requested,
                serial,
                began: false,
                running: preview.running.clone().or_else(|| {
                    self.machines
                        .iter()
                        .find(|update| update.machine == preview.machine)
                        .and_then(|update| update.facts.as_ref())
                        .map(|facts| facts.running.clone())
                }),
            });
            // The adapter's own deadline turns a restart that never came back into an
            // Unconfirmed link, which keeps the mark. This later one only ends a mark whose
            // restart never began while its Core answers as before.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(2 * RESTART_DEADLINE).await;
                let _ = this.update(cx, |this, cx| {
                    let quiet: Vec<MachineId> = this
                        .machines
                        .iter()
                        .filter(|update| {
                            matches!(
                                update.link,
                                MachineLink::Connected | MachineLink::Incompatible(_)
                            ) && update.facts.as_ref().is_some_and(|facts| {
                                !matches!(
                                    facts.operation,
                                    Some(
                                        Operation::Restarting { .. }
                                            | Operation::RestartingSameVersion
                                    )
                                )
                            })
                        })
                        .map(|update| update.machine)
                        .collect();
                    this.asked.retain(|work| {
                        work.serial != serial || work.began || !quiet.contains(&work.machine)
                    });
                    this.mark();
                    cx.notify();
                });
            })
            .detach();
            self.mark();
            cx.notify();
        }
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
        refused_at_once: bool,
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
            refused_at_once,
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
                        let failed_push = carried_failure(known, &update);
                        *known = update;
                        known.failed_push = failed_push;
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
        // An approved restart the Core refused without running it leaves no failure in its
        // facts, so the person waiting on the click hears it at once.
        let refused = action == MachineAction::Restart
            && matches!(
                outcome,
                Err(ActionFailure::Busy
                    | ActionFailure::Unreachable
                    | ActionFailure::Unavailable(_))
            );
        self.notify_outcome(machine, action, surface, outcome, refused, cx);
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
                // A new failed push shows its step again.
                self.dismissed.remove(&(machine, PUSH_FAILURE));
                let Some(version) = automatic.or_else(|| update.push.clone()).or_else(|| {
                    matches!(failure, ActionFailure::NeedsFloor(_))
                        .then(|| {
                            update
                                .bundled
                                .as_ref()
                                .map(|bundled| bundled.version.clone())
                        })
                        .flatten()
                }) else {
                    return;
                };
                if update.push.as_ref() == Some(&version)
                    || update.bundled.as_ref().map(|bundled| &bundled.version) == Some(&version)
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

/// The failed push `previous` recorded, while `next` still offers its version and its facts do
/// not settle it.
fn carried_failure(previous: &MachineUpdate, next: &MachineUpdate) -> Option<FailedPush> {
    previous.failed_push.clone().filter(|failed| {
        (next.push.as_ref() == Some(&failed.version)
            || next.bundled.as_ref().map(|bundled| &bundled.version) == Some(&failed.version))
            && !next
                .facts
                .as_ref()
                .is_some_and(|facts| failed.settled_by(facts))
    })
}

#[cfg(test)]
mod tests {
    use super::super::testing::ScriptedMachines;
    use super::*;
    use crate::updates::state::{self, Command, MachineStage, Step, Target};
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        FailureKind, InstallFailure, Operation, PendingVersion, Platform, Protocols, UpdateCheck,
        UpdateFacts, UpdateFailure,
    };

    fn update(running: &str, pending: Option<&str>) -> MachineUpdate {
        MachineUpdate {
            detailed: None,
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

    /// A check a person starts keeps its outcome, from the Core's facts after its answer,
    /// whichever arrives first, until a newer check changes those facts. A check that never ran
    /// fails at once.
    #[gpui::test]
    fn a_checks_outcome_is_the_facts_after_its_answer(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin.clone()]));
        cx.run_until_parked();
        let with_check = |revision: u64, check: UpdateCheck| {
            let mut next = penguin.clone();
            let facts = next.facts.as_mut().unwrap();
            facts.revision = revision;
            facts.check = check;
            MachineUpdatesEvent::Changed(Box::new(next))
        };
        let check_again = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::CheckAgain, Surface::Settings, cx);
            });
        };
        let answer = |outcome| MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::CheckAgain,
            outcome,
        };
        let asked = |cx: &mut gpui::TestAppContext| {
            cx.read(|cx| state::current(cx).machines[0].asked_check)
        };
        let failing = UpdateCheck::Failing {
            since: 5,
            last_success: None,
            reason: spaceterm_core::update::facts::CheckFailure::Network,
        };

        // The facts arrive first.
        check_again(cx);
        assert_eq!(asked(cx), Some(state::AskedCheck::Running));
        adapter.emit(with_check(2, failing.clone()));
        adapter.emit(answer(Ok(())));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Failed));

        // The answer arrives first: the next facts settle the outcome.
        check_again(cx);
        adapter.emit(answer(Ok(())));
        adapter.emit(with_check(3, UpdateCheck::Succeeded { at: 10 }));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Succeeded));

        // The Core's own later check replaces it.
        adapter.emit(with_check(4, UpdateCheck::Succeeded { at: 20 }));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        // A refused check never ran.
        check_again(cx);
        adapter.emit(answer(Err(ActionFailure::Busy)));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Failed));
    }

    /// A check that answered with an error stays the person's failure while the Core's facts
    /// carry no newer completed check: a revision that changes only sessions keeps it, a newer
    /// failing check keeps it as that failure, and a newer successful check clears it, in
    /// either order of the error reply and the facts.
    #[gpui::test]
    fn a_failed_check_lasts_until_a_newer_completed_check(cx: &mut gpui::TestAppContext) {
        let (adapter, store) = installed(cx);
        let penguin = update("0.8.0", None);
        let machine = penguin.machine;
        adapter.emit(MachineUpdatesEvent::Machines(vec![penguin.clone()]));
        cx.run_until_parked();
        let revised = |revision: u64, sessions: u32, check: UpdateCheck| {
            let mut next = penguin.clone();
            let facts = next.facts.as_mut().unwrap();
            facts.revision = revision;
            facts.sessions = sessions;
            facts.check = check;
            MachineUpdatesEvent::Changed(Box::new(next))
        };
        let check_again = |cx: &mut gpui::TestAppContext| {
            store.update(cx, |store, cx| {
                store.perform(machine, MachineAction::CheckAgain, Surface::Settings, cx);
            });
        };
        let failed = |failure| MachineUpdatesEvent::Finished {
            machine,
            action: MachineAction::CheckAgain,
            outcome: Err(failure),
        };
        let asked = |cx: &mut gpui::TestAppContext| {
            cx.read(|cx| state::current(cx).machines[0].asked_check)
        };
        let feed_down = UpdateCheck::Failing {
            since: 5,
            last_success: None,
            reason: spaceterm_core::update::facts::CheckFailure::Network,
        };

        // The Core's feed can't be reached: the reply comes first, then the failing facts.
        check_again(cx);
        adapter.emit(failed(ActionFailure::Unreachable));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Failed));
        adapter.emit(revised(2, 0, feed_down.clone()));
        cx.run_until_parked();
        assert_eq!(
            asked(cx),
            Some(state::AskedCheck::Failed),
            "the failure is still current"
        );
        adapter.emit(revised(3, 2, feed_down.clone()));
        cx.run_until_parked();
        assert_eq!(
            asked(cx),
            Some(state::AskedCheck::Failed),
            "sessions alone settle nothing"
        );
        adapter.emit(revised(4, 2, UpdateCheck::Succeeded { at: 20 }));
        cx.run_until_parked();
        assert_eq!(asked(cx), None, "a newer successful check clears it");

        // A busy Core refused the check, then its own scheduled check succeeds.
        check_again(cx);
        adapter.emit(failed(ActionFailure::Busy));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Failed));
        adapter.emit(revised(5, 0, UpdateCheck::Succeeded { at: 30 }));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);

        // The failing facts come before the reply.
        check_again(cx);
        adapter.emit(revised(6, 0, feed_down));
        adapter.emit(failed(ActionFailure::Unreachable));
        cx.run_until_parked();
        assert_eq!(asked(cx), Some(state::AskedCheck::Failed));
        adapter.emit(revised(7, 0, UpdateCheck::Succeeded { at: 40 }));
        cx.run_until_parked();
        assert_eq!(asked(cx), None);
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

        // The Core's own download then fails. That failure is not the person's either, and the
        // Core tries it again by itself, so it holds no step.
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
        assert_eq!(cx.update(|cx| state::current(cx).step), None);
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
        assert_eq!(
            store.read_with(cx, |store, _| store.machines()[0].asked),
            None
        );
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

        for failure in [ActionFailure::Unreachable, ActionFailure::DiskFull] {
            adapter.emit(MachineUpdatesEvent::Finished {
                machine: penguin,
                action: install.clone(),
                outcome: Err(failure),
            });
            adapter.emit(MachineUpdatesEvent::Changed(Box::new(idle())));
            cx.run_until_parked();
            let view = cx.update(|cx| state::current(cx));
            let row = view.machine_row(penguin).unwrap();
            assert_eq!(
                row.status.as_deref(),
                Some("Couldn\u{2019}t download 0.9.0")
            );
            let action = row.action.expect("the row offers Try Again");
            assert_eq!(action.label.as_ref(), "Try Again");
            assert_eq!(action.command, Command::Machine(penguin, install.clone()));
            assert_eq!(
                view.step,
                Some(Step::Failed(Target::Machine(penguin))),
                "the title bar shows it once"
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
        let view = cx.update(|cx| state::current(cx));
        assert_eq!(view.machine(penguin).unwrap().stage, MachineStage::Running);
        assert_eq!(view.step, None);
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
        let view = cx.update(|cx| state::current(cx));
        let row = view.machine_row(penguin).unwrap();
        assert_eq!(
            row.status.as_deref(),
            Some("Couldn\u{2019}t download 0.9.0")
        );
        let action = row.action.expect("the row offers Try Again");
        assert_eq!(action.label.as_ref(), "Try Again");
        assert_eq!(action.command, Command::Machine(penguin, install.clone()));
        assert_eq!(view.step, Some(Step::Failed(Target::Machine(penguin))));
        assert_eq!(adapter.performed().len(), 1, "nothing retries it");
    }

    #[gpui::test]
    fn failures_reach_only_the_surface_that_performed_them(cx: &mut gpui::TestAppContext) {
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

        let recent = cx.read(|cx| {
            cx.global::<crate::updates::UpdateService>()
                .0
                .read(cx)
                .recent()
                .len()
        });
        assert_eq!(recent, 0, "a finished update records nothing");
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
            "wait for the desktop\u{2019}s startup check"
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
