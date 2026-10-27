//! Private Terminal Session scheduling, including deadline arbitration and bounded fairness.
use super::publication::PublicationSchedule;
use super::*;
use crate::terminal::paste::PasteConfirmationSchedule;
use libghostty_vt::terminal::{CompressionActivity, CompressionResult};
use spaceterm_server::pane::{LineMode, LineState};
use std::sync::{Mutex, MutexGuard};

const LINE_SETTLE_INTERVAL: Duration = Duration::from_millis(200);
const ACCESSIBILITY_NORMAL_COMMAND_BURST: u8 = 8;
const ACCESSIBILITY_PRESENTATION_INTERVAL: Duration = Duration::from_millis(100);
/// How often a hidden Terminal Session reports changed metadata. A program that retitles its
/// Pane on every write would otherwise wake the server for every write.
const METADATA_PRESENTATION_INTERVAL: Duration = Duration::from_millis(100);
const COMPRESSION_IDLE_INTERVAL: Duration = Duration::from_millis(250);
const COMPRESSION_STEP_INTERVAL: Duration = Duration::from_millis(1);

struct CompressionSchedule<Activity> {
    activity: Option<Activity>,
    deadline: Option<Instant>,
    disabled: bool,
}

impl<Activity: Eq> CompressionSchedule<Activity> {
    fn new() -> Self {
        Self {
            activity: None,
            deadline: None,
            disabled: false,
        }
    }

    fn observe(&mut self, now: Instant, activity: Activity, postpone: bool) {
        if self.disabled {
            return;
        }
        if self.activity.as_ref() != Some(&activity) {
            self.activity = Some(activity);
            self.deadline = Some(now + COMPRESSION_IDLE_INTERVAL);
        } else if postpone && self.deadline.is_some() {
            self.deadline = Some(now + COMPRESSION_IDLE_INTERVAL);
        }
    }

    fn prime(&mut self, activity: Activity) {
        if !self.disabled {
            self.activity = Some(activity);
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            true
        } else {
            false
        }
    }

    fn complete(&mut self, now: Instant, result: CompressionResult) {
        match result {
            CompressionResult::Pending => self.deadline = Some(now + COMPRESSION_STEP_INTERVAL),
            CompressionResult::Complete => self.deadline = None,
            CompressionResult::Unsupported => self.stop(),
        }
    }

    fn stop(&mut self) {
        self.deadline = None;
        self.disabled = true;
    }
}

/// The Terminal Session handle can enqueue coalesced work without accessing worker schedules.
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

    #[cfg_attr(
        not(target_os = "macos"),
        allow(
            dead_code,
            reason = "only a native accessibility Adapter requests accessibility work"
        )
    )]
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

#[derive(Clone, Copy, Eq, PartialEq)]
enum PasteConfirmationOwner {
    Terminal,
    Dialog,
}

pub(super) struct WorkerSchedules {
    input: ScheduleInput,
    accessibility_continuation: AccessibilityContinuationSchedule,
    accessibility_presentation: AccessibilityPresentationSchedule,
    selection_autoscroll: SelectionAutoscrollSchedule,
    paste_confirmations: PasteConfirmationSchedule,
    paste_confirmation_owner: Option<(PasteConfirmationId, PasteConfirmationOwner)>,
    line: LineSchedule,
    presentation: PublicationSchedule,
    metadata_presentation_pending: bool,
    metadata_presentation_queued: Arc<AtomicBool>,
    /// The earliest time a hidden Terminal Session reports metadata again.
    metadata_not_before: Instant,
    graphics_animation: Option<Instant>,
    compression: CompressionSchedule<CompressionActivity>,
    clock: Clock,
}

/// Where publication reads the time. Tests fix it, so a slow run cannot change what coalesces.
#[derive(Clone, Copy)]
enum Clock {
    System,
    #[cfg(test)]
    Fixed(Instant),
}

impl WorkerSchedules {
    /// The time publication decisions use.
    pub(super) fn now(&self) -> Instant {
        match self.clock {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Fixed(now) => now,
        }
    }

    /// Fixes the time publication decisions use.
    #[cfg(test)]
    pub(super) fn fix_clock(&mut self, now: Instant) {
        self.clock = Clock::Fixed(now);
    }

    pub(super) fn subscribe(&mut self, now: Instant) {
        self.metadata_presentation_queued
            .store(false, Ordering::Release);
        self.metadata_presentation_pending = false;
        self.accessibility_continuation.update(false);
        self.input.accessibility_demand.clear();
        self.accessibility_presentation = AccessibilityPresentationSchedule::new(now);
        self.set_presentable(true, now);
        self.request_presentation();
    }

    pub(super) fn prime_compression(&mut self, activity: CompressionActivity) {
        self.compression.prime(activity);
    }

    pub(super) fn observe_compression(
        &mut self,
        now: Instant,
        activity: CompressionActivity,
        postpone: bool,
    ) {
        self.compression.observe(now, activity, postpone);
    }

    pub(super) fn complete_compression(&mut self, now: Instant, result: CompressionResult) {
        self.compression.complete(now, result);
    }

    pub(super) fn disable_compression(&mut self) {
        self.compression.stop();
    }

    pub(super) fn compression_due(&self, now: Instant) -> bool {
        self.compression
            .deadline()
            .is_some_and(|deadline| now >= deadline)
    }

    pub(super) fn take_compression_due(&mut self, now: Instant) -> bool {
        self.compression.take_due(now)
    }

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

    pub(super) fn update_selection_autoscroll(&mut self, now: Instant, interval: Option<Duration>) {
        self.selection_autoscroll
            .update(now, interval.filter(|_| self.presentation.is_presentable()));
    }

    pub(super) fn update_line(
        &mut self,
        now: Instant,
        result: Result<LineMode, NativePtyOperationFailure>,
    ) -> Option<LineState> {
        self.line.update(now, result)
    }

    pub(super) fn line_transition(&mut self, now: Instant) {
        self.line.transition(now);
    }

    pub(super) fn request_presentation(&mut self) {
        self.presentation.request();
    }

    pub(super) fn request_pty_presentation(&mut self, now: Instant) {
        self.presentation.output(now);
    }

    /// Sets the frame rate the presenting client declared, in hertz.
    pub(super) fn set_frame_rate(&mut self, hz: u8) {
        self.presentation.set_frame_rate(hz);
    }

    pub(super) fn note_metadata_changed(&mut self) {
        self.metadata_presentation_pending = true;
    }

    /// The wakeup for changed metadata, at most once per [`METADATA_PRESENTATION_INTERVAL`]. A
    /// change before then stays pending until [`Self::take_due`] reports it.
    pub(super) fn take_metadata_presentation(&mut self, now: Instant) -> Option<MetadataWakeup> {
        if !self.metadata_presentation_pending || now < self.metadata_not_before {
            return None;
        }
        self.metadata_presentation_pending = false;
        if self
            .metadata_presentation_queued
            .swap(true, Ordering::AcqRel)
        {
            return None;
        }
        self.metadata_not_before = now + METADATA_PRESENTATION_INTERVAL;
        Some(MetadataWakeup::new(Arc::clone(
            &self.metadata_presentation_queued,
        )))
    }

    /// When a hidden Terminal Session's pending metadata is next reported.
    fn metadata_deadline(&self) -> Option<Instant> {
        (self.metadata_presentation_pending && !self.is_presentable())
            .then_some(self.metadata_not_before)
    }

    pub(super) fn metadata_presented(&mut self) {
        self.metadata_presentation_pending = false;
    }

    pub(super) fn is_presentable(&self) -> bool {
        self.presentation.is_presentable()
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
        self.graphics_animation = deadline.filter(|_| self.presentation.is_presentable());
    }

    pub(super) fn set_presentable(&mut self, presentable: bool, now: Instant) {
        self.input.set_accessibility_demand_enabled(presentable);
        self.accessibility_presentation
            .set_presentable(presentable, now);
        if !presentable {
            self.accessibility_continuation.update(false);
            self.input.accessibility_demand.clear();
        }
        self.set_visual_presentable(presentable, now);
    }

    pub(super) fn set_server_presentable(&mut self, presentable: bool, now: Instant) {
        self.input.set_accessibility_demand_enabled(true);
        self.accessibility_presentation.set_presentable(true, now);
        self.set_visual_presentable(presentable, now);
    }

    fn set_visual_presentable(&mut self, presentable: bool, now: Instant) {
        self.presentation.set_presentable(presentable, now);
        if !presentable {
            if self.graphics_animation.take().is_some() {
                self.presentation.request();
            }
            self.selection_autoscroll.cancel();
        }
    }

    pub(super) fn note_hidden_server_content_changed(&mut self) {
        if !self.presentation.is_presentable()
            && self.accessibility_presentation.demand_request.is_some()
        {
            self.accessibility_presentation.note_screen_published();
        }
    }

    /// Ends native demand without changing visual presentation; a later demand reactivates it.
    /// A demand requested after `ended_at` survives, so a racing request is never lost.
    pub(super) fn end_accessibility_demand(&mut self, ended_at: Instant) {
        if self.input.accessibility_demand.clear_through(ended_at) {
            return;
        }
        self.accessibility_continuation.update(false);
        self.accessibility_presentation.end_demand();
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
        let confirmation = self.paste_confirmations.create(payload, now)?;
        self.paste_confirmation_owner = Some((confirmation.id, PasteConfirmationOwner::Terminal));
        Some(confirmation)
    }

    pub(super) fn resolve_paste_confirmation(
        &mut self,
        id: PasteConfirmationId,
        now: Instant,
    ) -> Option<PreparedPaste> {
        self.paste_confirmation_owner = None;
        self.paste_confirmations.take(id, now)
    }

    pub(super) fn focus_paste_confirmation(&mut self, id: PasteConfirmationId) -> bool {
        if self
            .paste_confirmation_owner
            .is_some_and(|(pending, _)| pending == id)
        {
            self.paste_confirmation_owner = Some((id, PasteConfirmationOwner::Dialog));
            true
        } else {
            false
        }
    }

    pub(super) fn paste_confirmation_has_focus(&self, id: PasteConfirmationId) -> bool {
        self.paste_confirmation_owner == Some((id, PasteConfirmationOwner::Dialog))
    }

    pub(super) fn cancel_paste_confirmation(&mut self) {
        self.paste_confirmation_owner = None;
        self.paste_confirmations.cancel();
    }

    pub(super) fn new(now: Instant, input: ScheduleInput) -> Self {
        Self {
            input,
            accessibility_continuation: AccessibilityContinuationSchedule::default(),
            accessibility_presentation: AccessibilityPresentationSchedule::new(now),
            selection_autoscroll: SelectionAutoscrollSchedule::default(),
            paste_confirmations: PasteConfirmationSchedule::default(),
            paste_confirmation_owner: None,
            line: LineSchedule::new(now),
            presentation: PublicationSchedule::new(now),
            metadata_presentation_pending: false,
            metadata_presentation_queued: Arc::new(AtomicBool::new(false)),
            metadata_not_before: now,
            graphics_animation: None,
            compression: CompressionSchedule::new(),
            clock: Clock::System,
        }
    }

    /// The earliest due work. A synchronized update (DEC 2026) holds publication until it ends
    /// or `synchronized_output`, the engine's timeout, passes.
    pub(super) fn deadline(&self, synchronized_output: Option<Instant>) -> Option<Instant> {
        [
            synchronized_output,
            self.selection_autoscroll.deadline(),
            self.paste_confirmations.deadline(),
            self.presentation
                .deadline()
                .filter(|_| synchronized_output.is_none()),
            self.accessibility_presentation.deadline(),
            self.metadata_deadline(),
            self.graphics_animation,
            self.compression.deadline(),
            self.line.deadline,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    pub(super) fn take_due(&mut self, now: Instant) -> Option<Command> {
        if self.selection_autoscroll.take_due(now) {
            return Some(Command::SelectionAutoscrollTick);
        }
        if self.paste_confirmations.expire(now) {
            self.paste_confirmation_owner = None;
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
        if self.metadata_deadline().is_some_and(|due| now >= due) {
            return Some(Command::PublishMetadata);
        }
        self.line
            .deadline
            .is_some_and(|deadline| now >= deadline)
            .then_some(Command::ReadLineMode)
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

    fn end_demand(&mut self) {
        self.pending = self.seed_required && self.pending;
        self.demand_request = None;
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

/// Reads the PTY's line mode after output, focus changes and written input. An idle session reads
/// nothing: a program that changes the mode silently is seen at the next of those, which is no
/// later than the person's first keystroke reaching it.
struct LineSchedule {
    state: LineState,
    deadline: Option<Instant>,
    settle: bool,
}

impl LineSchedule {
    fn new(now: Instant) -> Self {
        Self {
            state: LineState::default(),
            deadline: Some(now),
            settle: false,
        }
    }

    fn transition(&mut self, now: Instant) {
        self.deadline = Some(now);
        // Programs may write their prompt before changing termios. Read again once the
        // transition has settled.
        self.settle = true;
    }

    /// Returns the new state when the mode differs from the last one read.
    fn update(
        &mut self,
        now: Instant,
        result: Result<LineMode, NativePtyOperationFailure>,
    ) -> Option<LineState> {
        self.deadline = std::mem::take(&mut self.settle).then(|| now + LINE_SETTLE_INTERVAL);
        let mode = result.unwrap_or_else(|_| {
            eprintln!("PTY line mode inspection failed; releasing secure input");
            LineMode::UNKNOWN
        });
        if self.state.mode == mode {
            return None;
        }
        self.state = LineState {
            epoch: self.state.epoch + 1,
            mode,
        };
        Some(self.state)
    }
}

#[derive(Default)]
struct SelectionAutoscrollSchedule {
    deadline: Option<Instant>,
}

impl SelectionAutoscrollSchedule {
    fn update(&mut self, now: Instant, interval: Option<Duration>) {
        if let Some(interval) = interval {
            // Pointer motion updates the drag position without delaying its clock.
            self.deadline.get_or_insert(now + interval);
        } else {
            self.cancel();
        }
    }

    fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    fn cancel(&mut self) {
        self.deadline = None;
    }

    fn take_due(&mut self, now: Instant) -> bool {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            self.deadline = None;
            true
        } else {
            false
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

#[cfg_attr(
    not(target_os = "macos"),
    allow(
        dead_code,
        reason = "only a native accessibility Adapter requests accessibility work"
    )
)]
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

    /// Clears a request made at or before `instant`; returns whether a newer request remains.
    fn clear_through(&self, instant: Instant) -> bool {
        let mut state = self.lock();
        if state
            .latest_request
            .is_some_and(|requested_at| requested_at > instant)
        {
            return true;
        }
        state.latest_request = None;
        state.notified = false;
        false
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

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: LineMode = LineMode {
        canonical: true,
        echo: false,
        signals: true,
        shell_foreground: false,
    };

    #[test]
    fn compression_waits_for_a_quiet_interval_after_activity() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();

        schedule.observe(start, 1, false);

        assert!(!schedule.take_due(start + Duration::from_millis(249)));
        assert!(schedule.take_due(start + Duration::from_millis(250)));
        assert!(!schedule.take_due(start + Duration::from_millis(250)));
    }

    #[test]
    fn compression_postpones_pending_work_after_input_or_output() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();
        schedule.observe(start, 1, false);
        schedule.observe(start + Duration::from_millis(200), 1, true);

        assert!(!schedule.take_due(start + Duration::from_millis(250)));
        assert!(schedule.take_due(start + Duration::from_millis(450)));
    }

    #[test]
    fn compression_continues_in_bounded_steps_until_complete() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();
        schedule.observe(start, 1, false);
        assert!(schedule.take_due(start + Duration::from_millis(250)));

        schedule.complete(
            start + Duration::from_millis(250),
            CompressionResult::Pending,
        );
        assert!(!schedule.take_due(start + Duration::from_millis(250)));
        assert!(schedule.take_due(start + Duration::from_millis(251)));
        schedule.complete(
            start + Duration::from_millis(251),
            CompressionResult::Complete,
        );
        assert_eq!(schedule.deadline(), None);
    }

    #[test]
    fn completed_compression_restarts_only_when_activity_token_changes() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();
        schedule.observe(start, 1, false);
        assert!(schedule.take_due(start + Duration::from_millis(250)));
        schedule.complete(
            start + Duration::from_millis(250),
            CompressionResult::Complete,
        );

        schedule.observe(start + Duration::from_millis(300), 1, true);
        assert_eq!(schedule.deadline(), None);
        schedule.observe(start + Duration::from_millis(300), 2, false);
        assert_eq!(
            schedule.deadline(),
            Some(start + Duration::from_millis(550))
        );
    }

    #[test]
    fn unsupported_compression_stops_for_the_terminal_lifetime() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();
        schedule.observe(start, 1, false);
        assert!(schedule.take_due(start + Duration::from_millis(250)));
        schedule.complete(
            start + Duration::from_millis(250),
            CompressionResult::Unsupported,
        );

        schedule.observe(start + Duration::from_millis(300), 1, true);
        assert_eq!(schedule.deadline(), None);
        schedule.observe(start + Duration::from_millis(300), 2, false);
        assert_eq!(schedule.deadline(), None);
    }

    #[test]
    fn failed_compression_stops_for_the_terminal_lifetime() {
        let start = Instant::now();
        let mut schedule = CompressionSchedule::<u64>::new();
        schedule.observe(start, 1, false);

        schedule.stop();
        schedule.observe(start + Duration::from_millis(300), 2, false);

        assert_eq!(schedule.deadline(), None);
    }
    #[test]
    fn accessibility_continuation_runs_after_eight_normal_commands() {
        let mut schedule = AccessibilityContinuationSchedule::default();
        schedule.update(true);

        for command in 0..8 {
            assert!(!schedule.must_continue());
            schedule.note_normal_command();
            assert_eq!(schedule.must_continue(), command + 1 == 8);
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
    fn line_reads_emit_only_changes_with_a_new_epoch_and_fail_closed() {
        let start = Instant::now();
        let mut schedule = LineSchedule::new(start);

        assert_eq!(schedule.update(start, Ok(LineMode::UNKNOWN)), None);
        let changed = schedule.update(start, Ok(PASSWORD)).unwrap();
        assert_eq!((changed.epoch, changed.mode), (1, PASSWORD));
        assert_eq!(schedule.update(start, Ok(PASSWORD)), None);
        let failed = schedule
            .update(
                start,
                Err(NativePtyOperationFailure::new(
                    std::io::ErrorKind::BrokenPipe,
                )),
            )
            .unwrap();
        assert_eq!((failed.epoch, failed.mode), (2, LineMode::UNKNOWN));
        assert!(!failed.mode.hidden_input());
        assert_eq!(schedule.deadline, None);
    }

    #[test]
    fn line_reads_idle_without_a_deadline_after_one_transition_followup() {
        let now = Instant::now();
        let mut schedules = WorkerSchedules::new(now, ScheduleInput::default());
        schedules.update_line(now, Ok(LineMode::UNKNOWN));
        assert_eq!(schedules.deadline(None), None);
        assert!(
            schedules
                .take_due(now + Duration::from_secs(3600))
                .is_none()
        );

        let prompt = now + Duration::from_secs(1);
        schedules.line_transition(prompt);
        assert!(matches!(
            schedules.take_due(prompt),
            Some(Command::ReadLineMode)
        ));
        assert_eq!(schedules.update_line(prompt, Ok(LineMode::UNKNOWN)), None);
        let settled = prompt + Duration::from_millis(200);
        assert_eq!(schedules.deadline(None), Some(settled));
        assert!(matches!(
            schedules.take_due(settled),
            Some(Command::ReadLineMode)
        ));
        assert_eq!(
            schedules
                .update_line(settled, Ok(PASSWORD))
                .map(|state| state.mode),
            Some(PASSWORD)
        );
        assert_eq!(schedules.deadline(None), None);

        let focused = settled + Duration::from_secs(1);
        schedules.line_transition(focused);
        assert_eq!(schedules.deadline(None), Some(focused));
        assert_eq!(
            schedules
                .update_line(focused, Ok(LineMode::UNKNOWN))
                .map(|state| state.mode),
            Some(LineMode::UNKNOWN)
        );
    }

    #[test]
    fn selection_autoscroll_schedule_uses_an_injected_monotonic_now() {
        let epoch = Instant::now();
        let mut schedule = SelectionAutoscrollSchedule::default();

        schedule.update(epoch, Some(Duration::from_millis(100)));

        assert!(!schedule.take_due(epoch + Duration::from_millis(99)));
        assert!(schedule.take_due(epoch + Duration::from_millis(100)));
        assert!(!schedule.take_due(epoch + Duration::from_secs(1)));

        schedule.update(epoch, Some(Duration::from_millis(25)));
        schedule.update(epoch, None);
        assert!(!schedule.take_due(epoch + Duration::from_secs(1)));
    }

    #[test]
    fn selection_autoscroll_motion_does_not_postpone_the_next_tick() {
        let epoch = Instant::now();
        let interval = Duration::from_millis(15);
        let mut schedule = SelectionAutoscrollSchedule::default();
        schedule.update(epoch, Some(interval));
        for millis in 1..15 {
            schedule.update(epoch + Duration::from_millis(millis), Some(interval));
        }
        assert!(schedule.take_due(epoch + interval));
        assert!(!schedule.take_due(epoch + interval));
        schedule.update(epoch + interval, Some(interval));
        assert!(!schedule.take_due(epoch + Duration::from_millis(29)));
        assert!(schedule.take_due(epoch + Duration::from_millis(30)));
    }

    #[test]
    fn worker_schedules_preserve_deadline_priority_and_expire_each_payload_once() {
        let now = Instant::now();
        let mut schedules = WorkerSchedules::new(now, ScheduleInput::default());
        schedules.update_line(now, Ok(LineMode::UNKNOWN));
        let due = now + Duration::from_secs(30);
        // Output just before `due` asks for a line mode read.
        schedules.line_transition(due);
        schedules.update_selection_autoscroll(now, Some(Duration::from_secs(30)));
        schedules
            .request_paste_confirmation(PreparedPaste::prepare("one\ntwo".into()).unwrap(), now)
            .unwrap();
        assert_eq!(schedules.deadline(Some(now)), Some(now));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::SelectionAutoscrollTick)
        ));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::PasteConfirmationExpired)
        ));
        assert!(matches!(
            schedules.take_due(due),
            Some(Command::ReadLineMode)
        ));
        schedules.update_line(due, Ok(LineMode::UNKNOWN));
        assert!(schedules.take_due(due).is_none());
    }

    #[test]
    fn synchronized_output_holds_publication_until_the_engine_deadline() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.update_line(start, Ok(LineMode::UNKNOWN));
        schedules.mark_presented(start);
        let output = start + Duration::from_secs(1);
        schedules.request_pty_presentation(output);
        assert!(schedules.deadline(None).is_some_and(|due| due <= output));

        let timeout = output + Duration::from_millis(150);
        assert_eq!(schedules.deadline(Some(timeout)), Some(timeout));
    }

    #[test]
    fn graphics_animation_wakes_once_at_the_engine_deadline() {
        let start = Instant::now();
        let mut schedules = WorkerSchedules::new(start, ScheduleInput::default());
        schedules.update_line(start, Ok(LineMode::UNKNOWN));
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
        schedules.update_line(start, Ok(LineMode::UNKNOWN));
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
    fn presentation_barrier_consumes_pending_work_in_visible_and_hidden_states() {
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
        schedules.update_line(start, Ok(LineMode::UNKNOWN));
        schedules.update_accessibility(true);
        schedules.update_selection_autoscroll(start, Some(Duration::from_millis(20)));

        schedules.set_presentable(false, start);

        assert!(!schedules.accessibility_pending());
        assert!(!matches!(
            schedules.take_due(start + Duration::from_millis(20)),
            Some(Command::SelectionAutoscrollTick)
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
        schedules.update_line(start, Ok(LineMode::UNKNOWN));
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
    fn ending_accessibility_demand_stops_refreshes_until_a_later_demand() {
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

        schedules.end_accessibility_demand(start + Duration::from_millis(1));
        schedules.note_screen_published();
        assert!(!schedules.accessibility_presentation_due(start + Duration::from_secs(60)));

        let later = start + Duration::from_secs(60);
        assert!(input.enqueue_accessibility_demand(later));
        schedules.accessibility_demand_received(later);
        assert!(schedules.accessibility_presentation_due(later));
    }

    #[test]
    fn ending_accessibility_demand_keeps_a_newer_demand() {
        let start = Instant::now();
        let input = ScheduleInput::default();
        let mut schedules = WorkerSchedules::new(start, input.clone());
        schedules.note_screen_published();
        schedules.mark_accessibility_presented(start, true);

        let ended_at = start + Duration::from_millis(1);
        assert!(input.enqueue_accessibility_demand(ended_at + Duration::from_millis(1)));
        schedules.end_accessibility_demand(ended_at);
        schedules.accessibility_demand_received(ended_at + Duration::from_millis(2));
        assert!(schedules.accessibility_presentation_due(ended_at + Duration::from_millis(2)));
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
