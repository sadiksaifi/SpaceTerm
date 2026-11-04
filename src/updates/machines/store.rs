//! The application's one view of Machine updates: it starts the composed
//! [`MachineUpdatesAdapter`], keeps the latest [`MachineUpdate`] per Machine, the failure notices
//! dismissed on this desktop and the Recent history, and performs actions. Observers of
//! [`MachineUpdatesService`] hear every change. [`MachineUpdatesNotice`] reports a refused or
//! failed action so the surface that started it can explain it.

use std::collections::VecDeque;
use std::rc::Rc;

use gpui::{App, AppContext, Context, Entity, EventEmitter, Global, SharedString, Task};
use spaceterm_core::update::facts::Version;

use super::{
    ActionFailure, MachineAction, MachineId, MachineUpdate, MachineUpdatesAdapter,
    MachineUpdatesEvent,
};
use crate::updates::presentation::Dismissed;

/// The most entries Recent keeps.
pub(crate) const RECENT_LIMIT: usize = 4;

/// One finished update in Recent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecentUpdate {
    /// "penguin updated to 0.9.0", "SpaceTerm updated to 0.9.0" or "penguin installed 0.9.0".
    pub(crate) text: SharedString,
    /// Unix seconds.
    pub(crate) at: u64,
}

/// An action that did not succeed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MachineUpdatesNotice {
    pub(crate) machine: MachineId,
    pub(crate) name: SharedString,
    pub(crate) local: bool,
    pub(crate) action: MachineAction,
    pub(crate) failure: ActionFailure,
}

pub(crate) struct MachineUpdates {
    adapter: Rc<dyn MachineUpdatesAdapter>,
    machines: Vec<MachineUpdate>,
    dismissed: Dismissed,
    recent: VecDeque<RecentUpdate>,
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
                    if this
                        .update(cx, |this, cx| this.receive(event, cx))
                        .is_err()
                    {
                        return;
                    }
                }
            });
            Self {
                adapter,
                machines: Vec::new(),
                dismissed: Dismissed::new(),
                recent: VecDeque::new(),
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

    /// Newest first.
    pub(crate) fn recent(&self) -> impl Iterator<Item = &RecentUpdate> {
        self.recent.iter()
    }

    /// Asks the adapter to perform `action`. Dismissing a failure retires it here at once.
    pub(crate) fn perform(
        &mut self,
        machine: MachineId,
        action: MachineAction,
        cx: &mut Context<Self>,
    ) {
        if let MachineAction::DismissFailure(id) = action {
            self.dismissed.insert((machine, id));
            cx.notify();
        }
        self.adapter.perform(machine, action);
    }

    /// Adds `text` to Recent, such as "SpaceTerm updated to 0.9.0".
    pub(crate) fn record(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.push_recent(text.into());
        cx.notify();
    }

    fn push_recent(&mut self, text: SharedString) {
        self.recent.push_front(RecentUpdate {
            text,
            at: crate::updates::now(),
        });
        self.recent.truncate(RECENT_LIMIT);
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
                            self.push_recent(text);
                        }
                    }
                    None => self.machines.push(update),
                }
            }
            MachineUpdatesEvent::Finished {
                machine,
                action,
                outcome: Err(failure),
            } => {
                if let MachineAction::DismissFailure(_) = action {
                    return;
                }
                let Some(update) = self.machines.iter().find(|known| known.machine == machine)
                else {
                    return;
                };
                cx.emit(MachineUpdatesNotice {
                    machine,
                    name: update.name.clone(),
                    local: update.local,
                    action,
                    failure,
                });
            }
            MachineUpdatesEvent::Finished { .. } => {}
        }
        cx.notify();
    }
}

/// What Recent says when `next` finishes an update `previous` was waiting for.
fn finished_update(previous: &MachineUpdate, next: &MachineUpdate) -> Option<SharedString> {
    let (before, after) = (previous.facts.as_ref()?, next.facts.as_ref()?);
    let name = &next.name;
    if before.running != after.running && is_newer(&after.running, &before.running) {
        return Some(format!("{name} updated to {}", after.running).into());
    }
    match (&before.pending, &after.pending) {
        (None, Some(pending)) => Some(format!("{name} installed {}", pending.version).into()),
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
        adapter.emit(MachineUpdatesEvent::Changed(Box::new(update("0.9.0", None))));
        adapter.emit(MachineUpdatesEvent::Finished {
            machine: [2; 32],
            action: MachineAction::Restart,
            outcome: Err(ActionFailure::Busy),
        });
        cx.run_until_parked();

        let recent: Vec<SharedString> = cx.read(|cx| {
            store
                .read(cx)
                .recent()
                .map(|entry| entry.text.clone())
                .collect()
        });
        assert_eq!(
            recent,
            vec![
                SharedString::from("penguin updated to 0.9.0"),
                SharedString::from("penguin installed 0.9.0"),
            ]
        );
        assert_eq!(notices.borrow().len(), 1);
        assert_eq!(notices.borrow()[0].failure, ActionFailure::Busy);

        cx.update(|cx| {
            store.update(cx, |store, cx| {
                store.perform([2; 32], MachineAction::DismissFailure(4), cx);
            });
        });
        assert!(cx.read(|cx| store.read(cx).dismissed().contains(&([2; 32], 4))));
        assert_eq!(
            adapter.performed(),
            vec![([2; 32], MachineAction::DismissFailure(4))]
        );
    }
}
