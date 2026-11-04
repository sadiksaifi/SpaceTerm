//! A scripted adapter for tests: the test emits each event and reads back the actions performed.
use std::cell::RefCell;

use super::{
    MachineAction, MachineId, MachineUpdatesAdapter, MachineUpdatesError, MachineUpdatesEvent,
};

#[derive(Default)]
pub(crate) struct ScriptedMachines {
    events: RefCell<Option<async_channel::Sender<MachineUpdatesEvent>>>,
    performed: RefCell<Vec<(MachineId, MachineAction)>>,
}

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
}
