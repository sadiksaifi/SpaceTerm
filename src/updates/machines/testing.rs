//! A scripted adapter for tests: the test emits each event and reads back the actions performed.
use std::cell::RefCell;

use super::{
    ActionFailure, MachineAction, MachineId, MachinePreview, MachineUpdatesAdapter,
    MachineUpdatesError, MachineUpdatesEvent, PreviewAnswer,
};

#[derive(Default)]
pub(crate) struct ScriptedMachines {
    events: RefCell<Option<async_channel::Sender<MachineUpdatesEvent>>>,
    performed: RefCell<Vec<(MachineId, MachineAction)>>,
    previews: RefCell<Vec<(MachineId, MachineAction, PreviewSender)>>,
    executed: RefCell<Vec<MachinePreview>>,
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

    fn preview(&self, machine: MachineId, action: MachineAction) -> PreviewAnswer {
        let (answer, answered) = async_channel::bounded(1);
        self.previews.borrow_mut().push((machine, action, answer));
        answered
    }

    fn execute(&self, preview: MachinePreview) {
        self.executed.borrow_mut().push(preview);
    }
}

impl ScriptedMachines {
    /// Sends `event` as the adapter. Panics before `start`.
    pub(crate) fn emit(&self, event: MachineUpdatesEvent) {
        self.events
            .borrow()
            .as_ref()
            .expect("the adapter started")
            .try_send(event)
            .expect("the events channel is open");
    }

    /// Every action performed so far, in order.
    pub(crate) fn performed(&self) -> Vec<(MachineId, MachineAction)> {
        self.performed.borrow().clone()
    }

    /// Answers the oldest unanswered preview with `answer`. Panics when none waits.
    pub(crate) fn answer_preview(
        &self,
        answer: impl FnOnce(MachineId, MachineAction) -> Result<MachinePreview, ActionFailure>,
    ) {
        let (machine, action, sender) = self.previews.borrow_mut().remove(0);
        let _ = sender.try_send(answer(machine, action));
    }

    /// Every preview executed so far, in order.
    pub(crate) fn executed(&self) -> Vec<MachinePreview> {
        self.executed.borrow().clone()
    }
}
