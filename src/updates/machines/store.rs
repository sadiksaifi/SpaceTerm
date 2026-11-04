//! The application's one view of Machine updates: it starts the composed
//! [`MachineUpdatesAdapter`], keeps the latest [`MachineUpdate`] per Machine and the failure
//! notices dismissed on this desktop, records each finished update in Recent, and performs
//! actions. Observers of [`MachineUpdatesService`] hear every change. [`MachineUpdatesNotice`]
//! reports how each action a surface performed finished, so that surface, and only it, can
//! explain a failure.

use std::rc::Rc;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use spaceterm_core::update::facts::Version;

use super::{
    ActionFailure, MachineAction, MachineId, MachinePreview, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::presentation::{self, Confirmation, Dismissed};

/// Where the person asked for an action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Surface {
    /// The title-bar control, its alerts and notifications.
    Window,
    /// The menu bar extra or the Linux status item.
    StatusMenu,
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
    /// A preview failed, and its surface heard why.
    Nothing,
}

/// Shows a confirmation on a surface and passes on whether the person approved it.
pub(crate) type Ask = Rc<dyn Fn(Confirmation, Box<dyn FnOnce(bool, &mut App)>, &mut App)>;

/// Restarts or stops `actions`' Machines only on their Cores' own previews: each Core previews
/// first, the person confirms the sessions that Core reports through `ask`, and exactly those
/// previews execute. A Machine whose Core reports no facts is previewed like any other.
/// `always` asks even when no session closes, as after a Core refused a preview: a refused
/// action never runs again without a fresh confirmation. An approval that no longer holds
/// against the facts previews again.
pub(crate) fn request(
    actions: Vec<(MachineId, MachineAction)>,
    always: bool,
    surface: Surface,
    ask: Ask,
    cx: &mut App,
) {
    let Some(store) = MachineUpdates::global(cx) else {
        return;
    };
    let prepared = store.update(cx, |store, cx| store.prepare(&actions, always, surface, cx));
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
                        request(actions, false, surface, again, cx);
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

    /// Asks each affected Core to preview its Restart or Stop. A preview that fails reports
    /// that failure to `surface` as the action's outcome and prepares nothing.
    fn prepare(
        &self,
        actions: &[(MachineId, MachineAction)],
        always: bool,
        surface: Surface,
        cx: &mut Context<Self>,
    ) -> Task<Prepared> {
        let answers: Vec<_> = actions
            .iter()
            .map(|(machine, action)| {
                let answer = self.adapter.preview(*machine, action.clone());
                (*machine, action.clone(), answer)
            })
            .collect();
        cx.spawn(async move |this, cx| {
            let mut previews = Vec::new();
            for (machine, action, answer) in answers {
                match answer.recv().await {
                    Ok(Ok(preview)) => previews.push(preview),
                    Ok(Err(failure)) => {
                        let _ = this.update(cx, |this, cx| {
                            this.notify_outcome(machine, action, surface, Err(failure), cx);
                        });
                        return Prepared::Nothing;
                    }
                    Err(_) => return Prepared::Nothing,
                }
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
            MachineUpdatesEvent::Machines(machines) => self.machines = machines,
            MachineUpdatesEvent::Changed(update) => {
                let update = *update;
                match self
                    .machines
                    .iter_mut()
                    .find(|known| known.machine == update.machine)
                {
                    Some(known) => {
                        let finished = finished_update(known, &update);
                        *known = update;
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
        }
        cx.notify();
    }
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
    use spaceterm_core::protocol::capability::{Capabilities, Capability};
    use spaceterm_core::update::facts::{
        PendingVersion, Platform, Protocols, UpdateCheck, UpdateFacts,
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
        }
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
