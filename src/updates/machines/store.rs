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

use std::collections::HashSet;
use std::rc::Rc;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use spaceterm_core::update::facts::Version;

use super::{
    ActionFailure, FailedPush, MachineAction, MachineId, MachineLink, MachinePreview,
    MachineUpdate, MachineUpdatesAdapter, MachineUpdatesEvent,
};
use crate::updates::presentation::{self, Approval, Confirmation, Dismissed};

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
    /// Each bundled Core version installed automatically in this run, by Machine.
    offered: HashSet<(MachineId, Version)>,
    /// Machines whose automatic install has not finished, and the version it pushes. Its
    /// finish reaches no surface, and a failure stays in the Machine's `failed_push`.
    installing: Vec<(MachineId, Version)>,
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
                offered: HashSet::new(),
                installing: Vec::new(),
                _events: events,
            }
        });
        cx.set_global(MachineUpdatesService(entity));
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
        if let MachineAction::DismissFailure(id) = action {
            self.dismissed.insert((machine, id));
            cx.notify();
        }
        self.performed.push((machine, action.clone(), surface));
        self.adapter.perform(machine, action);
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
                    self.finish_performed(machine, action, outcome, cx);
                }
            }
        }
        self.install_bundled();
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
                let Some(version) = automatic.or_else(|| update.push.clone()) else {
                    return;
                };
                if update.push.as_ref() == Some(&version) {
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
    fn install_bundled(&mut self) {
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
    previous
        .failed_push
        .clone()
        .filter(|failed| next.push.as_ref() == Some(&failed.version))
}

/// What Recent says when `next` finishes an update `previous` was waiting for.
fn finished_update(previous: &MachineUpdate, next: &MachineUpdate) -> Option<String> {
    let (before, after) = (previous.facts.as_ref()?, next.facts.as_ref()?);
    let name = &next.name;
    if before.running != after.running && is_newer(&after.running, &before.running) {
        return Some(format!("{name} updated to {}", after.running));
    }
    match (&before.pending, &after.pending) {
        (None, Some(pending)) => Some(format!("{name} installed {}", pending.version)),
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
        Operation, PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts,
    };

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
        }
    }

    fn offered(update: MachineUpdate, push: &str) -> MachineUpdate {
        MachineUpdate {
            push: Some(Version::parse(push).unwrap()),
            ..update
        }
    }

    /// The bundled Core installs with no click on each Machine offered it, once per Machine and
    /// version, only while the Machine answers and runs no update operation, and a failure
    /// reaches no surface and is not retried.
    #[gpui::test]
    fn the_bundled_core_installs_once_per_machine_and_version(cx: &mut gpui::TestAppContext) {
        let updater = Rc::new(crate::updates::testing::RecordingAdapter::available());
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater, cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
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
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater, cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
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
            assert_eq!(view.control_label(), "penguin install failed");
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
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater, cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
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
        assert_eq!(view.control_label(), "penguin install failed");
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
        cx.update(|cx| crate::updates::ApplicationUpdates::install(updater, cx));
        let adapter = Rc::new(ScriptedMachines::default());
        cx.update(|cx| MachineUpdates::install(adapter.clone(), cx));
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
}
