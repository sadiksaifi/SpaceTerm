//! A scripted adapter for tests: the test emits each event and reads back the actions performed.
//! Each preview is answered at once with the sessions and the Pending Version of the latest facts
//! the test emitted for its Machine, as a Core whose preview matches its facts, unless the test
//! holds previews to answer them itself.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use spaceterm_core::update::facts::Version;

use super::{
    ActionFailure, MachineAction, MachineId, MachinePreview, MachineUpdatesAdapter,
    MachineUpdatesError, MachineUpdatesEvent, PreviewAnswer, Request,
};

#[derive(Default)]
pub(crate) struct ScriptedMachines {
    events: RefCell<Option<async_channel::Sender<MachineUpdatesEvent>>>,
    performed: RefCell<Vec<(MachineId, MachineAction)>>,
    /// The sessions and Pending Version each Machine's latest emitted facts report.
    reported: RefCell<HashMap<MachineId, (u32, Option<Version>)>>,
    hold: Cell<bool>,
    previews: RefCell<Vec<(MachineId, MachineAction, PreviewSender)>>,
    asked: RefCell<Vec<(MachineId, MachineAction)>>,
    executed: RefCell<Vec<MachinePreview>>,
    /// The store's request of each execution and each preview, in order.
    requests: RefCell<Vec<(MachineId, MachineAction, Request)>>,
}

type PreviewSender = async_channel::Sender<Result<MachinePreview, ActionFailure>>;

impl MachineUpdatesAdapter for ScriptedMachines {
    fn start(
        &self,
        events: async_channel::Sender<MachineUpdatesEvent>,
    ) -> Result<(), MachineUpdatesError> {
        *self.events.borrow_mut() = Some(events);
        Ok(())
    }

    fn perform(&self, machine: MachineId, action: MachineAction) {
        self.performed.borrow_mut().push((machine, action));
    }

    fn preview(
        &self,
        machine: MachineId,
        action: MachineAction,
        request: Request,
    ) -> PreviewAnswer {
        let (answer, answered) = async_channel::bounded(1);
        self.asked.borrow_mut().push((machine, action.clone()));
        self.requests
            .borrow_mut()
            .push((machine, action.clone(), request));
        if self.hold.get() {
            self.previews.borrow_mut().push((machine, action, answer));
        } else {
            let (sessions, pending) = self
                .reported
                .borrow()
                .get(&machine)
                .cloned()
                .unwrap_or_default();
            let target = pending.filter(|_| action == MachineAction::Restart);
            let _ = answer.try_send(Ok(MachinePreview {
                target,
                ..preview(machine, action, sessions)
            }));
        }
        answered
    }

    fn execute(&self, preview: MachinePreview, request: Request) {
        self.requests
            .borrow_mut()
            .push((preview.machine, preview.action.clone(), request));
        self.executed.borrow_mut().push(preview);
    }
}

/// A preview of `action` that ends `sessions`.
pub(crate) fn preview(machine: MachineId, action: MachineAction, sessions: u32) -> MachinePreview {
    MachinePreview {
        machine,
        action,
        sessions,
        target: None,
        running: None,
        id: 0,
    }
}

impl ScriptedMachines {
    /// The store's event channel, for an event sent off the test's thread, as the adapter's
    /// followers send them. It records nothing the scripted previews read.
    pub(crate) fn events(&self) -> Option<async_channel::Sender<MachineUpdatesEvent>> {
        self.events.borrow().clone()
    }

    /// Sends `event` as the adapter. Panics before `start`.
    pub(crate) fn emit(&self, event: MachineUpdatesEvent) {
        let reported = match &event {
            MachineUpdatesEvent::Machines(machines) => machines.iter().collect(),
            MachineUpdatesEvent::Changed(update) => vec![update.as_ref()],
            MachineUpdatesEvent::Finished { .. } | MachineUpdatesEvent::Waiting { .. } => {
                Vec::new()
            }
        };
        for update in reported {
            let facts = update.facts.as_ref();
            let sessions = facts.map_or(0, |facts| facts.sessions);
            let pending = facts
                .and_then(|facts| facts.pending.as_ref())
                .map(|pending| pending.version.clone());
            self.reported
                .borrow_mut()
                .insert(update.machine, (sessions, pending));
        }
        self.events
            .borrow()
            .as_ref()
            .expect("the adapter started")
            .try_send(event)
            .expect("the events channel is open");
    }

    /// The store's request of the latest preview or execution, which the test names in the
    /// events it emits for it. Panics when there is none.
    pub(crate) fn last_request(&self) -> Request {
        self.requests.borrow().last().expect("a request").2
    }

    /// Every action performed so far, in order.
    pub(crate) fn performed(&self) -> Vec<(MachineId, MachineAction)> {
        self.performed.borrow().clone()
    }

    /// Leaves each later preview unanswered until [`Self::answer_preview`].
    pub(crate) fn hold_previews(&self) {
        self.hold.set(true);
    }

    /// Answers the oldest unanswered preview with `answer`. Panics when none waits.
    pub(crate) fn answer_preview(
        &self,
        answer: impl FnOnce(MachineId, MachineAction) -> Result<MachinePreview, ActionFailure>,
    ) {
        let (machine, action, sender) = self.previews.borrow_mut().remove(0);
        let _ = sender.try_send(answer(machine, action));
    }

    /// Answers the unanswered preview of `machine` with `answer`. Panics when none waits.
    pub(crate) fn answer_preview_of(
        &self,
        machine: MachineId,
        answer: impl FnOnce(MachineAction) -> Result<MachinePreview, ActionFailure>,
    ) {
        let index = self
            .previews
            .borrow()
            .iter()
            .position(|(waiting, ..)| *waiting == machine)
            .expect("a preview of the Machine waits");
        let (_, action, sender) = self.previews.borrow_mut().remove(index);
        let _ = sender.try_send(answer(action));
    }

    /// Every preview asked for so far, in order.
    pub(crate) fn asked(&self) -> Vec<(MachineId, MachineAction)> {
        self.asked.borrow().clone()
    }

    /// Every preview executed so far, in order.
    pub(crate) fn executed(&self) -> Vec<MachinePreview> {
        self.executed.borrow().clone()
    }

    /// The Machine and action of every preview executed so far, in order.
    pub(crate) fn executed_actions(&self) -> Vec<(MachineId, MachineAction)> {
        self.executed
            .borrow()
            .iter()
            .map(|preview| (preview.machine, preview.action.clone()))
            .collect()
    }
}
