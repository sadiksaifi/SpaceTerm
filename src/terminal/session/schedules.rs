//! Private Session scheduling, including deadline arbitration and bounded fairness.
use super::*;

pub(super) struct WorkerSchedules {
    pub(super) accessibility_continuation: AccessibilityContinuationSchedule,
    pub(super) selection_autoscroll: SelectionAutoscrollSchedule,
    pub(super) paste_confirmations: PasteConfirmationSchedule,
    pub(super) osc52_authorization: Osc52AuthorizationSchedule,
    pub(super) hidden_input: HiddenInputSchedule,
}

impl WorkerSchedules {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            accessibility_continuation: AccessibilityContinuationSchedule::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            osc52_authorization: Osc52AuthorizationSchedule::default(),
            hidden_input: HiddenInputSchedule::new(now),
        }
    }

    pub(super) fn deadline(&self, synchronized_output: Option<Instant>) -> Option<Instant> {
        [
            synchronized_output,
            self.selection_autoscroll.deadline(),
            self.paste_confirmations.deadline(),
            self.osc52_authorization.deadline(),
            Some(self.hidden_input.deadline),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<Command> {
        if let Some(generation) = self.selection_autoscroll.take_due(now) {
            return Some(Command::SelectionAutoscrollTick(generation));
        }
        if self.paste_confirmations.expire(now) {
            return Some(Command::PasteConfirmationExpired);
        }
        if let Some(id) = self.osc52_authorization.expire(now) {
            return Some(Command::Osc52AuthorizationExpired(id));
        }
        (now >= self.hidden_input.deadline).then_some(Command::PollHiddenInput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_schedules_preserve_deadline_priority_and_expire_each_payload_once() {
        let now = Instant::now();
        let mut schedules = WorkerSchedules::new(now);
        schedules.hidden_input.update(now, Ok(false));
        let due = now + Duration::from_secs(30);
        schedules.selection_autoscroll.update(
            now,
            Some(Duration::from_secs(30)),
            PresentationGeneration::default(),
        );
        schedules
            .paste_confirmations
            .create(PreparedPaste::prepare("one\ntwo".into()).unwrap(), now)
            .unwrap();
        assert_eq!(schedules.deadline(Some(now)), Some(now));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::SelectionAutoscrollTick(_))
        ));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::PasteConfirmationExpired)
        ));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::PollHiddenInput)
        ));
        schedules.hidden_input.update(due, Ok(false));
        assert!(schedules.take_due(due).is_none());
    }
}

#[derive(Default)]
pub(super) struct AccessibilityContinuationSchedule {
    pub(super) pending: bool,
    pub(super) normal_commands: u8,
}

impl AccessibilityContinuationSchedule {
    pub(super) fn update(&mut self, more: bool) {
        self.pending = more;
        if !more {
            self.normal_commands = 0;
        }
    }

    pub(super) fn note_normal_command(&mut self) {
        if self.pending {
            self.normal_commands = self.normal_commands.saturating_add(1);
        }
    }

    pub(super) fn must_continue(&self) -> bool {
        self.pending && self.normal_commands >= ACCESSIBILITY_NORMAL_COMMAND_BURST
    }

    pub(super) fn take(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        self.pending = false;
        self.normal_commands = 0;
        true
    }
}

pub(super) struct HiddenInputSchedule {
    active: bool,
    pub(super) deadline: Instant,
}

impl HiddenInputSchedule {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            active: false,
            deadline: now,
        }
    }

    pub(super) fn update(
        &mut self,
        now: Instant,
        result: Result<bool, NativePtyOperationFailure>,
    ) -> Option<bool> {
        self.deadline = now + HIDDEN_INPUT_POLL_INTERVAL;
        let active = match result {
            Ok(active) => active,
            Err(error) => {
                eprintln!(
                    "failed to inspect PTY hidden-input state; releasing secure input: {error}"
                );
                false
            }
        };
        if self.active == active {
            None
        } else {
            self.active = active;
            Some(active)
        }
    }
}

#[derive(Default)]
pub(super) struct SelectionAutoscrollSchedule {
    pub(super) deadline: Option<Instant>,
    generation: PresentationGeneration,
}

impl SelectionAutoscrollSchedule {
    pub(super) fn update(
        &mut self,
        now: Instant,
        interval: Option<Duration>,
        generation: PresentationGeneration,
    ) {
        self.deadline = interval.map(|interval| now + interval);
        self.generation = generation;
    }

    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<PresentationGeneration> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            Some(self.generation)
        } else {
            None
        }
    }
}
