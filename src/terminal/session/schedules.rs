//! Private Session scheduling, including deadline arbitration and bounded fairness.
use super::*;
use crate::terminal::paste::PasteConfirmationSchedule;
use std::sync::{Mutex, MutexGuard};

const HIDDEN_INPUT_POLL_INTERVAL: Duration = Duration::from_millis(200);
const ACCESSIBILITY_NORMAL_COMMAND_BURST: u8 = 8;

/// The Session handle can enqueue coalesced work without accessing worker schedules.
#[derive(Clone, Default)]
pub(super) struct ScheduleInput {
    resizes: ResizeMailbox,
    find_queries: FindQueryMailbox,
}

impl ScheduleInput {
    pub(super) fn enqueue_resize(&self, geometry: TerminalGeometry) -> bool {
        self.resizes.replace(geometry)
    }

    pub(super) fn enqueue_find_query(
        &self,
        generation: FindQueryGeneration,
        query: String,
    ) -> bool {
        self.find_queries
            .replace(FindQueryUpdate::Set(generation, query))
    }

    pub(super) fn enqueue_find_end(&self, generation: FindQueryGeneration) -> bool {
        self.find_queries.replace(FindQueryUpdate::End(generation))
    }
}

pub(super) struct WorkerSchedules {
    input: ScheduleInput,
    accessibility_continuation: AccessibilityContinuationSchedule,
    selection_autoscroll: SelectionAutoscrollSchedule,
    paste_confirmations: PasteConfirmationSchedule,
    hidden_input: HiddenInputSchedule,
}

impl WorkerSchedules {
    pub(super) fn take_resize(&mut self) -> Option<TerminalGeometry> {
        self.input.resizes.take()
    }

    pub(super) fn take_find_query(&mut self) -> Option<FindQueryUpdate> {
        self.input.find_queries.take()
    }

    pub(super) fn update_accessibility(&mut self, more: bool) {
        self.accessibility_continuation.update(more);
    }

    pub(super) fn accessibility_pending(&self) -> bool {
        self.accessibility_continuation.pending
    }

    pub(super) fn must_continue_accessibility(&self) -> bool {
        self.accessibility_continuation.must_continue()
    }

    pub(super) fn note_normal_command(&mut self) {
        self.accessibility_continuation.note_normal_command();
    }

    pub(super) fn take_accessibility_continuation(&mut self) -> bool {
        self.accessibility_continuation.take()
    }

    pub(super) fn update_selection_autoscroll(
        &mut self,
        now: Instant,
        interval: Option<Duration>,
        generation: PresentationGeneration,
    ) {
        self.selection_autoscroll.update(now, interval, generation);
    }

    pub(super) fn update_hidden_input(
        &mut self,
        now: Instant,
        result: Result<bool, NativePtyOperationFailure>,
    ) -> Option<bool> {
        self.hidden_input.update(now, result)
    }

    pub(super) fn request_paste_confirmation(
        &mut self,
        payload: PreparedPaste,
        now: Instant,
    ) -> Option<crate::terminal::paste::PasteConfirmation> {
        self.paste_confirmations.create(payload, now)
    }

    pub(super) fn resolve_paste_confirmation(
        &mut self,
        id: PasteConfirmationId,
        now: Instant,
    ) -> Option<PreparedPaste> {
        self.paste_confirmations.take(id, now)
    }

    pub(super) fn cancel_paste_confirmation(&mut self) {
        self.paste_confirmations.cancel();
    }

    pub(super) fn new(now: Instant, input: ScheduleInput) -> Self {
        Self {
            input,
            accessibility_continuation: AccessibilityContinuationSchedule::default(),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            hidden_input: HiddenInputSchedule::new(now),
        }
    }

    pub(super) fn deadline(&self, synchronized_output: Option<Instant>) -> Option<Instant> {
        [
            synchronized_output,
            self.selection_autoscroll.deadline(),
            self.paste_confirmations.deadline(),
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
        (now >= self.hidden_input.deadline).then_some(Command::PollHiddenInput)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accessibility_continuation_runs_after_eight_normal_commands() {
        let mut schedule = AccessibilityContinuationSchedule::default();
        schedule.update(true);

        for command in 0..ACCESSIBILITY_NORMAL_COMMAND_BURST {
            assert!(!schedule.must_continue());
            schedule.note_normal_command();
            assert_eq!(
                schedule.must_continue(),
                command + 1 == ACCESSIBILITY_NORMAL_COMMAND_BURST
            );
        }

        assert!(schedule.take());
        assert!(!schedule.pending);
        assert_eq!(schedule.normal_commands, 0);
    }

    #[test]
    fn accessibility_continuation_is_cancelled_by_a_complete_update() {
        let mut schedule = AccessibilityContinuationSchedule::default();
        schedule.update(true);
        schedule.note_normal_command();
        schedule.update(false);

        assert!(!schedule.pending);
        assert!(!schedule.must_continue());
        assert!(!schedule.take());
    }

    #[test]
    fn repeated_incomplete_observations_do_not_starve_continuation_fairness() {
        let mut schedule = AccessibilityContinuationSchedule::default();
        schedule.update(true);

        for _ in 0..ACCESSIBILITY_NORMAL_COMMAND_BURST {
            schedule.note_normal_command();
            schedule.update(true);
        }

        assert!(schedule.must_continue());
    }

    #[test]
    fn hidden_input_polling_emits_only_transitions_and_fails_closed() {
        let start = Instant::now();
        let mut schedule = HiddenInputSchedule::new(start);

        assert_eq!(schedule.update(start, Ok(false)), None);
        assert_eq!(schedule.update(start, Ok(true)), Some(true));
        assert_eq!(schedule.update(start, Ok(true)), None);
        assert_eq!(
            schedule.update(
                start,
                Err(NativePtyOperationFailure::new(
                    "descriptor closed".to_owned()
                ))
            ),
            Some(false)
        );
        assert_eq!(schedule.deadline, start + HIDDEN_INPUT_POLL_INTERVAL);
    }

    #[test]
    fn selection_autoscroll_schedule_uses_an_injected_monotonic_now() {
        let epoch = Instant::now();
        let generation = PresentationGeneration::default();
        let mut schedule = SelectionAutoscrollSchedule::default();

        schedule.update(epoch, Some(Duration::from_millis(100)), generation);

        assert_eq!(schedule.take_due(epoch + Duration::from_millis(99)), None);
        assert_eq!(
            schedule.take_due(epoch + Duration::from_millis(100)),
            Some(generation)
        );
        assert_eq!(schedule.take_due(epoch + Duration::from_secs(1)), None);

        schedule.update(epoch, Some(Duration::from_millis(25)), generation);
        schedule.update(epoch, None, generation);
        assert_eq!(schedule.take_due(epoch + Duration::from_secs(1)), None);
    }

    #[test]
    fn worker_schedules_preserve_deadline_priority_and_expire_each_payload_once() {
        let now = Instant::now();
        let mut schedules = WorkerSchedules::new(now, ScheduleInput::default());
        schedules.update_hidden_input(now, Ok(false));
        let due = now + Duration::from_secs(30);
        schedules.update_selection_autoscroll(
            now,
            Some(Duration::from_secs(30)),
            PresentationGeneration::default(),
        );
        schedules
            .request_paste_confirmation(PreparedPaste::prepare("one\ntwo".into()).unwrap(), now)
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
        schedules.update_hidden_input(due, Ok(false));
        assert!(schedules.take_due(due).is_none());
    }
}

#[derive(Default)]
struct AccessibilityContinuationSchedule {
    pending: bool,
    normal_commands: u8,
}

impl AccessibilityContinuationSchedule {
    fn update(&mut self, more: bool) {
        self.pending = more;
        if !more {
            self.normal_commands = 0;
        }
    }

    fn note_normal_command(&mut self) {
        if self.pending {
            self.normal_commands = self.normal_commands.saturating_add(1);
        }
    }

    fn must_continue(&self) -> bool {
        self.pending && self.normal_commands >= ACCESSIBILITY_NORMAL_COMMAND_BURST
    }

    fn take(&mut self) -> bool {
        if !self.pending {
            return false;
        }
        self.pending = false;
        self.normal_commands = 0;
        true
    }
}

struct HiddenInputSchedule {
    active: bool,
    deadline: Instant,
}

impl HiddenInputSchedule {
    fn new(now: Instant) -> Self {
        Self {
            active: false,
            deadline: now,
        }
    }

    fn update(
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
struct SelectionAutoscrollSchedule {
    deadline: Option<Instant>,
    generation: PresentationGeneration,
}

impl SelectionAutoscrollSchedule {
    fn update(
        &mut self,
        now: Instant,
        interval: Option<Duration>,
        generation: PresentationGeneration,
    ) {
        self.deadline = interval.map(|interval| now + interval);
        self.generation = generation;
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn take_due(&mut self, now: Instant) -> Option<PresentationGeneration> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            Some(self.generation)
        } else {
            None
        }
    }
}

#[derive(Clone, Default)]
struct ResizeMailbox {
    pending: Arc<Mutex<Option<TerminalGeometry>>>,
}

impl ResizeMailbox {
    fn replace(&self, geometry: TerminalGeometry) -> bool {
        let mut pending = self.lock();
        let should_notify = pending.is_none();
        *pending = Some(geometry);
        should_notify
    }

    fn take(&self) -> Option<TerminalGeometry> {
        self.lock().take()
    }

    fn lock(&self) -> MutexGuard<'_, Option<TerminalGeometry>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal resize mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
}

#[derive(Debug)]
pub(super) enum FindQueryUpdate {
    Set(FindQueryGeneration, String),
    End(FindQueryGeneration),
}

#[derive(Clone, Default)]
struct FindQueryMailbox {
    pending: Arc<Mutex<Option<FindQueryUpdate>>>,
}

impl FindQueryMailbox {
    fn replace(&self, update: FindQueryUpdate) -> bool {
        let mut pending = self.lock();
        let should_notify = pending.is_none();
        *pending = Some(update);
        should_notify
    }

    fn take(&self) -> Option<FindQueryUpdate> {
        self.lock().take()
    }

    fn lock(&self) -> MutexGuard<'_, Option<FindQueryUpdate>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal Find mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
}
