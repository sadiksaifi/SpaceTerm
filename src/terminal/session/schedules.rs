//! Private Session scheduling, including deadline arbitration and bounded fairness.
use super::*;
use crate::terminal::paste::PasteConfirmationSchedule;
use std::sync::{Mutex, MutexGuard};

const HIDDEN_INPUT_SETTLE_INTERVAL: Duration = Duration::from_millis(200);
const HIDDEN_INPUT_IDLE_INTERVAL: Duration = Duration::from_secs(30);
const ACCESSIBILITY_NORMAL_COMMAND_BURST: u8 = 8;
const PRESENTATION_INTERVAL: Duration = Duration::from_micros(16_667);
const ACCESSIBILITY_PRESENTATION_INTERVAL: Duration = Duration::from_millis(100);

/// The Session handle can enqueue coalesced work without accessing worker schedules.
#[derive(Clone, Default)]
pub(super) struct ScheduleInput {
    resizes: ResizeMailbox,
    find_queries: FindQueryMailbox,
    accessibility_demand: AccessibilityDemandMailbox,
    terminal_appearance: TerminalAppearanceMailbox,
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

    pub(super) fn enqueue_accessibility_demand(&self, requested_at: Instant) -> bool {
        self.accessibility_demand.request(requested_at)
    }

    pub(super) fn set_accessibility_demand_enabled(&self, enabled: bool) {
        self.accessibility_demand.set_enabled(enabled);
    }

    pub(super) fn enqueue_terminal_appearance(&self, update: TerminalAppearanceUpdate) -> bool {
        self.terminal_appearance.replace(update)
    }
}

pub(super) struct WorkerSchedules {
    input: ScheduleInput,
    accessibility_continuation: AccessibilityContinuationSchedule,
    accessibility_presentation: AccessibilityPresentationSchedule,
    selection_autoscroll: SelectionAutoscrollSchedule,
    paste_confirmations: PasteConfirmationSchedule,
    hidden_input: HiddenInputSchedule,
    presentation: PresentationSchedule,
    graphics_animation: Option<Instant>,
}

impl WorkerSchedules {
    pub(super) fn take_terminal_appearance(&mut self) -> Option<TerminalAppearanceUpdate> {
        self.input.terminal_appearance.take()
    }

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

    pub(super) fn note_screen_published(&mut self) {
        self.accessibility_presentation.note_screen_published();
    }

    pub(super) fn accessibility_demand_received(&mut self, now: Instant) {
        let Some(requested_at) = self.input.accessibility_demand.latest_request() else {
            return;
        };
        if !self
            .accessibility_presentation
            .activate_demand(requested_at, now)
        {
            self.input.accessibility_demand.clear();
        }
    }

    #[cfg(test)]
    pub(super) fn accessibility_presentation_due(&mut self, now: Instant) -> bool {
        self.accessibility_presentation.take_due(now)
    }

    pub(super) fn mark_accessibility_presented(&mut self, now: Instant, complete: bool) {
        self.accessibility_presentation
            .mark_presented(now, complete);
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

    pub(super) fn hidden_input_transition(&mut self, now: Instant) {
        self.hidden_input.transition(now);
    }

    pub(super) fn request_presentation(&mut self) {
        self.presentation.request();
    }

    pub(super) fn presentation_due(&mut self, now: Instant) -> bool {
        self.presentation.take_due(now)
    }

    pub(super) fn take_presentation_barrier(&mut self) -> bool {
        self.presentation.take_pending()
    }

    pub(super) fn take_visible_presentation(&mut self) -> bool {
        self.presentation.take_visible_pending()
    }

    pub(super) fn mark_presented(&mut self, now: Instant) {
        self.presentation.mark_presented(now);
    }

    pub(super) fn update_graphics_animation(&mut self, deadline: Option<Instant>) {
        self.graphics_animation = deadline.filter(|_| self.presentation.presentable);
    }

    pub(super) fn set_presentable(&mut self, presentable: bool, now: Instant) {
        self.input.set_accessibility_demand_enabled(presentable);
        self.presentation.set_presentable(presentable, now);
        self.accessibility_presentation
            .set_presentable(presentable, now);
        if !presentable {
            if self.graphics_animation.take().is_some() {
                self.presentation.request();
            }
            self.accessibility_continuation.update(false);
            self.input.accessibility_demand.clear();
            self.selection_autoscroll.cancel();
        }
    }

    pub(super) fn disable_accessibility(&mut self) {
        self.accessibility_continuation.update(false);
        self.accessibility_presentation.disable();
        self.input.accessibility_demand.clear();
        self.input.set_accessibility_demand_enabled(false);
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
            accessibility_presentation: AccessibilityPresentationSchedule::new(now),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            hidden_input: HiddenInputSchedule::new(now),
            presentation: PresentationSchedule::new(now),
            graphics_animation: None,
        }
    }

    pub(super) fn deadline(&self, synchronized_output: Option<Instant>) -> Option<Instant> {
        [
            synchronized_output,
            self.selection_autoscroll.deadline(),
            self.paste_confirmations.deadline(),
            self.presentation.deadline(),
            self.accessibility_presentation.deadline(),
            self.graphics_animation,
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
        if self.presentation.take_due(now) {
            return Some(Command::PublishPendingScreen);
        }
        if self
            .graphics_animation
            .is_some_and(|deadline| now >= deadline)
        {
            self.graphics_animation = None;
            return Some(Command::GraphicsAnimationTick);
        }
        if self.accessibility_presentation.take_due(now) {
            return Some(Command::PublishAccessibility);
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
        assert_eq!(schedule.deadline, start + HIDDEN_INPUT_IDLE_INTERVAL);
    }

    #[test]
    fn hidden_input_idles_for_thirty_seconds_after_one_transition_followup() {
        let now = Instant::now();
        let mut schedules = WorkerSchedules::new(now, ScheduleInput::default());
        schedules.update_hidden_input(now, Ok(false));
        assert_eq!(
            schedules.deadline(None),
            Some(now + Duration::from_secs(30))
        );
        assert!(schedules.take_due(now + Duration::from_secs(29)).is_none());

        let prompt = now + Duration::from_secs(1);
        schedules.hidden_input_transition(prompt);
        assert!(matches!(
            schedules.take_due(prompt),
            Some(Command::PollHiddenInput)
        ));
        assert_eq!(schedules.update_hidden_input(prompt, Ok(false)), None);
        let settled = prompt + Duration::from_millis(200);
        assert_eq!(schedules.deadline(None), Some(settled));
        assert!(matches!(
            schedules.take_due(settled),
            Some(Command::PollHiddenInput)
        ));
        assert_eq!(schedules.update_hidden_input(settled, Ok(true)), Some(true));
        assert_eq!(
            schedules.deadline(None),
            Some(settled + Duration::from_secs(30))
        );

        let focused = settled + Duration::from_secs(1);
        schedules.hidden_input_transition(focused);
        assert_eq!(schedules.deadline(None), Some(focused));
        assert_eq!(
            schedules.update_hidden_input(focused, Ok(false)),
            Some(false)
        );
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

    #[test]
    fn presentation_schedule_coalesces_repeated_requests_to_one_display_interval() {
        let start = Instant::now();
        let mut schedule = PresentationSchedule::new(start);

        schedule.request();
        assert!(schedule.take_due(start));
        schedule.mark_presented(start);

        schedule.request();
        schedule.request();
        assert_eq!(schedule.deadline(), Some(start + PRESENTATION_INTERVAL));
        assert!(!schedule.take_due(start + PRESENTATION_INTERVAL - Duration::from_micros(1)));
        assert!(schedule.take_due(start + PRESENTATION_INTERVAL));
        assert!(!schedule.take_due(start + PRESENTATION_INTERVAL));
    }

    #[test]
    fn graphics_animation_wakes_once_at_the_engine_deadline() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.update_hidden_input(start, Ok(false));
        let due = start + Duration::from_millis(40);
        schedules.update_graphics_animation(Some(due));

        assert_eq!(schedules.deadline(None), Some(due));
        assert!(schedules.take_due(due - Duration::from_millis(1)).is_none());
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::GraphicsAnimationTick)
        ));
        assert!(schedules.take_due(due).is_none());
    }

    #[test]
    fn hidden_graphics_stop_waking_and_resume_with_a_fresh_presentation() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.update_hidden_input(start, Ok(false));
        schedules.update_graphics_animation(Some(start + Duration::from_millis(40)));
        schedules.set_presentable(false, start);

        assert!(
            schedules
                .take_due(start + Duration::from_millis(40))
                .is_none()
        );
        let shown = start + Duration::from_millis(80);
        schedules.set_presentable(true, shown);
        assert!(matches!(
            schedules.take_due(shown),
            Some(Command::PublishPendingScreen)
        ));
    }

    #[test]
    fn hidden_presentation_retains_only_pending_work_and_restores_it_immediately() {
        let start = Instant::now();
        let mut schedule = PresentationSchedule::new(start);
        schedule.mark_presented(start);
        schedule.set_presentable(false, start);

        schedule.request();
        schedule.request();
        assert_eq!(schedule.deadline(), None);
        assert!(!schedule.take_due(start + Duration::from_secs(1)));

        let restored = start + Duration::from_secs(1);
        schedule.set_presentable(true, restored);
        assert_eq!(schedule.deadline(), Some(restored));
        assert!(schedule.take_due(restored));
        assert!(!schedule.take_due(restored));
    }

    #[test]
    fn presentation_barrier_flushes_visible_work_before_generation_sensitive_input() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.mark_presented(start);
        schedules.request_presentation();

        assert!(schedules.take_presentation_barrier());
        assert!(!schedules.take_presentation_barrier());

        schedules.set_presentable(false, start);
        schedules.request_presentation();
        assert!(schedules.take_presentation_barrier());
    }

    #[test]
    fn hidden_presentation_cancels_background_accessibility_and_autoscroll_work() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.update_hidden_input(start, Ok(false));
        schedules.update_accessibility(true);
        schedules.update_selection_autoscroll(
            start,
            Some(Duration::from_millis(20)),
            PresentationGeneration::default(),
        );

        schedules.set_presentable(false, start);

        assert!(!schedules.accessibility_pending());
        assert!(!matches!(
            schedules.take_due(start + Duration::from_millis(20)),
            Some(Command::SelectionAutoscrollTick(_))
        ));
    }

    #[test]
    fn accessibility_presentation_is_seeded_then_paced_only_during_native_demand() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, true);

        schedules.note_screen_published();
        assert!(!schedules.accessibility_presentation_due(start + Duration::from_secs(1)));

        schedules
            .input
            .enqueue_accessibility_demand(start + Duration::from_secs(1));
        schedules.accessibility_demand_received(start + Duration::from_secs(1));
        assert!(schedules.accessibility_presentation_due(start + Duration::from_secs(1)));
        schedules.mark_accessibility_presented(start + Duration::from_secs(1), true);

        schedules.note_screen_published();
        schedules.note_screen_published();
        assert!(!schedules.accessibility_presentation_due(
            start + Duration::from_secs(1) + ACCESSIBILITY_PRESENTATION_INTERVAL
                - Duration::from_micros(1)
        ));
        assert!(schedules.accessibility_presentation_due(
            start + Duration::from_secs(1) + ACCESSIBILITY_PRESENTATION_INTERVAL
        ));
    }

    #[test]
    fn accessibility_demand_mailbox_coalesces_one_visible_lifetime_activation() {
        let start = Instant::now();
        let input = ScheduleInput::default();
        let mut schedules = WorkerSchedules::new(start, input.clone());
        schedules.update_hidden_input(start, Ok(false));
        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, true);

        assert!(input.enqueue_accessibility_demand(start));
        assert!(!input.enqueue_accessibility_demand(start + Duration::from_millis(400)));
        schedules.accessibility_demand_received(start + Duration::from_millis(400));
        assert!(schedules.accessibility_presentation_due(start + Duration::from_millis(400)));
        schedules.mark_accessibility_presented(start + Duration::from_millis(400), true);

        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start + Duration::from_secs(60)));
        assert!(!input.enqueue_accessibility_demand(start + Duration::from_secs(60)));
    }

    #[test]
    fn restoring_visibility_reuses_a_complete_accessibility_cache_until_demanded() {
        let start = Instant::now();
        let input = ScheduleInput::default();
        let mut schedules = WorkerSchedules::new(start, input.clone());
        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, true);

        schedules.set_presentable(false, start + ACCESSIBILITY_PRESENTATION_INTERVAL);
        assert!(!schedules.accessibility_presentation_due(start + Duration::from_secs(1)));
        schedules.set_presentable(true, start + Duration::from_secs(1));
        assert!(!schedules.accessibility_presentation_due(start + Duration::from_secs(1)));
        assert!(input.enqueue_accessibility_demand(start + Duration::from_secs(1)));
        schedules.accessibility_demand_received(start + Duration::from_secs(1));
        assert!(schedules.accessibility_presentation_due(start + Duration::from_secs(1)));
    }

    #[test]
    fn incomplete_accessibility_seed_survives_a_new_screen_generation() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, false);

        schedules.note_screen_published();
        assert!(!schedules.accessibility_presentation_due(
            start + ACCESSIBILITY_PRESENTATION_INTERVAL - Duration::from_micros(1)
        ));
        assert!(
            schedules.accessibility_presentation_due(start + ACCESSIBILITY_PRESENTATION_INTERVAL)
        );
    }

    #[test]
    fn hiding_clears_accessibility_demand_and_pending_refreshes() {
        let start = Instant::now();
        let input = ScheduleInput::default();
        let mut schedules = WorkerSchedules::new(start, input.clone());
        schedules.note_screen_published();
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, true);

        assert!(input.enqueue_accessibility_demand(start));
        schedules.accessibility_demand_received(start);
        assert!(schedules.accessibility_presentation_due(start));
        schedules.mark_accessibility_presented(start, true);
        schedules.note_screen_published();

        schedules.set_presentable(false, start + Duration::from_millis(50));
        assert!(!schedules.accessibility_presentation_due(start + Duration::from_secs(60)));
        assert!(!input.enqueue_accessibility_demand(start + Duration::from_secs(60)));
    }

    #[test]
    fn terminal_appearance_mailbox_retains_only_the_latest_update() {
        let input = ScheduleInput::default();
        let mut schedules = WorkerSchedules::new(Instant::now(), input.clone());
        let mut update = crate::terminal::test_terminal_appearance_update();
        update.generation = AppearanceGeneration::new(1);
        assert!(input.enqueue_terminal_appearance(update));

        for generation in 2..=3 {
            let mut update = crate::terminal::test_terminal_appearance_update();
            update.generation = AppearanceGeneration::new(generation);
            assert!(!input.enqueue_terminal_appearance(update));
        }

        assert_eq!(
            schedules.take_terminal_appearance().unwrap().generation,
            AppearanceGeneration::new(3)
        );
        let mut next = crate::terminal::test_terminal_appearance_update();
        next.generation = AppearanceGeneration::new(4);
        assert!(input.enqueue_terminal_appearance(next));
    }
}

struct AccessibilityPresentationSchedule {
    presentable: bool,
    pending: bool,
    seed_required: bool,
    not_before: Instant,
    demand_request: Option<Instant>,
}

impl AccessibilityPresentationSchedule {
    fn new(now: Instant) -> Self {
        Self {
            presentable: true,
            pending: false,
            seed_required: true,
            not_before: now,
            demand_request: None,
        }
    }

    fn note_screen_published(&mut self) {
        if self.seed_required || self.demand_request.is_some() {
            self.pending = true;
        }
    }

    fn activate_demand(&mut self, requested_at: Instant, now: Instant) -> bool {
        if !self.presentable {
            return false;
        }
        let was_inactive = self.demand_request.is_none();
        self.demand_request = Some(requested_at);
        if was_inactive {
            self.pending = true;
            self.not_before = now;
        }
        true
    }

    fn set_presentable(&mut self, presentable: bool, now: Instant) {
        if presentable && !self.presentable {
            self.not_before = now;
            self.pending = self.seed_required;
        } else if !presentable {
            self.pending = false;
            self.demand_request = None;
        }
        self.presentable = presentable;
    }

    fn mark_presented(&mut self, now: Instant, complete: bool) {
        self.pending = false;
        if complete {
            self.seed_required = false;
        }
        self.not_before = now + ACCESSIBILITY_PRESENTATION_INTERVAL;
    }

    fn disable(&mut self) {
        self.pending = false;
        self.seed_required = false;
        self.demand_request = None;
    }

    fn deadline(&self) -> Option<Instant> {
        if !self.presentable {
            return None;
        }
        self.pending.then_some(self.not_before)
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.presentable && self.pending && now >= self.not_before {
            self.pending = false;
            true
        } else {
            false
        }
    }
}

struct PresentationSchedule {
    presentable: bool,
    pending: bool,
    not_before: Instant,
}

impl PresentationSchedule {
    fn new(now: Instant) -> Self {
        Self {
            presentable: true,
            pending: false,
            not_before: now,
        }
    }

    fn request(&mut self) {
        self.pending = true;
    }

    fn set_presentable(&mut self, presentable: bool, now: Instant) {
        if presentable && !self.presentable {
            self.not_before = now;
        }
        self.presentable = presentable;
    }

    fn mark_presented(&mut self, now: Instant) {
        self.pending = false;
        self.not_before = now + PRESENTATION_INTERVAL;
    }

    fn deadline(&self) -> Option<Instant> {
        (self.presentable && self.pending).then_some(self.not_before)
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline().is_some_and(|deadline| now >= deadline) {
            self.take_pending()
        } else {
            false
        }
    }

    fn take_pending(&mut self) -> bool {
        if self.pending {
            self.pending = false;
            true
        } else {
            false
        }
    }

    fn take_visible_pending(&mut self) -> bool {
        self.presentable && self.take_pending()
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
    settle: bool,
}

impl HiddenInputSchedule {
    fn new(now: Instant) -> Self {
        Self {
            active: false,
            deadline: now,
            settle: false,
        }
    }

    fn transition(&mut self, now: Instant) {
        self.deadline = now;
        // Programs may write their prompt before changing termios. Check again once the
        // transition has settled, then return to the long fallback for silent changes.
        self.settle = true;
    }

    fn update(
        &mut self,
        now: Instant,
        result: Result<bool, NativePtyOperationFailure>,
    ) -> Option<bool> {
        self.deadline = now
            + if std::mem::take(&mut self.settle) {
                HIDDEN_INPUT_SETTLE_INTERVAL
            } else {
                HIDDEN_INPUT_IDLE_INTERVAL
            };
        let active = match result {
            Ok(active) => active,
            Err(_) => {
                eprintln!("PTY hidden-input inspection failed; releasing secure input");
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

    fn cancel(&mut self) {
        self.deadline = None;
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

#[derive(Clone, Default)]
struct TerminalAppearanceMailbox {
    pending: Arc<Mutex<Option<TerminalAppearanceUpdate>>>,
}

impl TerminalAppearanceMailbox {
    fn replace(&self, update: TerminalAppearanceUpdate) -> bool {
        let mut pending = self.lock();
        let should_notify = pending.is_none();
        *pending = Some(update);
        should_notify
    }

    fn take(&self) -> Option<TerminalAppearanceUpdate> {
        self.lock().take()
    }

    fn lock(&self) -> MutexGuard<'_, Option<TerminalAppearanceUpdate>> {
        self.pending.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal appearance mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
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

#[derive(Clone, Default)]
struct AccessibilityDemandMailbox {
    state: Arc<Mutex<AccessibilityDemandMailboxState>>,
}

struct AccessibilityDemandMailboxState {
    latest_request: Option<Instant>,
    notified: bool,
    enabled: bool,
}

impl Default for AccessibilityDemandMailboxState {
    fn default() -> Self {
        Self {
            latest_request: None,
            notified: false,
            enabled: true,
        }
    }
}

impl AccessibilityDemandMailbox {
    fn request(&self, requested_at: Instant) -> bool {
        let mut state = self.lock();
        if !state.enabled {
            return false;
        }
        state.latest_request = Some(requested_at);
        if state.notified {
            false
        } else {
            state.notified = true;
            true
        }
    }

    fn latest_request(&self) -> Option<Instant> {
        self.lock().latest_request
    }

    fn clear(&self) {
        let mut state = self.lock();
        state.latest_request = None;
        state.notified = false;
    }

    fn set_enabled(&self, enabled: bool) {
        let mut state = self.lock();
        state.enabled = enabled;
        if !enabled {
            state.latest_request = None;
            state.notified = false;
        }
    }

    fn lock(&self) -> MutexGuard<'_, AccessibilityDemandMailboxState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            eprintln!("terminal accessibility demand mailbox recovered after a worker panic");
            poisoned.into_inner()
        })
    }
}
