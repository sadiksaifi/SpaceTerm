use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::geometry::TerminalGeometry;

pub(crate) const RUNTIME_TRANSITION_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub(crate) struct RuntimeObservation {
    state: Arc<RuntimeObservationState>,
}

#[derive(Debug)]
struct RuntimeObservationState {
    worker_generation: AtomicU64,
    screens_published: AtomicU64,
    screens_enqueued: AtomicU64,
    screens_superseded: AtomicU64,
    event_queue_length: AtomicU64,
    event_queue_high_water: AtomicU64,
    ui_dispatches: AtomicU64,
    ui_screen_events: AtomicU64,
    ui_drain_high_water: AtomicU64,
    ui_latest_generation: AtomicU64,
    render_latest_generation: AtomicU64,
    next_frame_generation: AtomicU64,
    next_frame_count: AtomicU64,
    presentable: AtomicBool,
    minimized: AtomicBool,
    occluded: AtomicBool,
    workspace_visible: AtomicBool,
    pane_visible: AtomicBool,
    live_resize: AtomicBool,
    visibility_version: AtomicU64,
    viewport_version: AtomicU64,
    viewport_total_rows: AtomicU64,
    viewport_visible_rows: AtomicU64,
    viewport_offset_rows: AtomicU64,
    selection_present: AtomicBool,
    resize_requests: AtomicU64,
    resize_notifications: AtomicU64,
    resize_applied: AtomicU64,
    resize_coalesced: AtomicU64,
    pty_geometry_version: AtomicU64,
    pty_rows: AtomicU64,
    pty_columns: AtomicU64,
    pty_pixel_width: AtomicU64,
    pty_pixel_height: AtomicU64,
    terminal_inputs_accepted: AtomicU64,
    lifecycle: AtomicU8,
    observer_drops: AtomicU64,
    observer_failed: AtomicBool,
    sealed: AtomicBool,
    ui_attached: AtomicBool,
    presentable_known: AtomicU8,
    restore_pending_generation: AtomicU64,
    next_event_sequence: AtomicU64,
    transitions: Mutex<VecDeque<RuntimeTransition>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum RuntimeLifecycle {
    Starting = 0,
    Running = 1,
    Exited = 2,
    Failed = 3,
    ObserverFailed = 4,
}

impl RuntimeLifecycle {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Failed => "failed",
            Self::ObserverFailed => "observer-failed",
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Running,
            2 => Self::Exited,
            3 => Self::Failed,
            4 => Self::ObserverFailed,
            _ => Self::Starting,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeEventKind {
    VisibilityLost,
    VisibilityRestored,
    FirstNextFrameAfterRestore,
    SessionExited,
    SessionFailed,
    ObserverFailed,
}

impl RuntimeEventKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::VisibilityLost => "visibility-lost",
            Self::VisibilityRestored => "visibility-restored",
            Self::FirstNextFrameAfterRestore => "first-next-frame-after-restore",
            Self::SessionExited => "session-exited",
            Self::SessionFailed => "session-failed",
            Self::ObserverFailed => "observer-failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeTransition {
    pub(crate) sequence: u64,
    pub(crate) continuous_ns: u64,
    pub(crate) kind: RuntimeEventKind,
    pub(crate) generation: u64,
    pub(crate) aux0: u64,
    pub(crate) aux1: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSample {
    pub(crate) continuous_ns: u64,
    pub(crate) worker_generation: u64,
    pub(crate) screens_published: u64,
    pub(crate) screens_enqueued: u64,
    pub(crate) screens_superseded: u64,
    pub(crate) event_queue_length: u64,
    pub(crate) event_queue_high_water: u64,
    pub(crate) ui_dispatches: u64,
    pub(crate) ui_screen_events: u64,
    pub(crate) ui_drain_high_water: u64,
    pub(crate) ui_latest_generation: u64,
    pub(crate) render_latest_generation: u64,
    pub(crate) next_frame_generation: u64,
    pub(crate) next_frame_count: u64,
    pub(crate) presentable: bool,
    pub(crate) minimized: bool,
    pub(crate) occluded: bool,
    pub(crate) workspace_visible: bool,
    pub(crate) pane_visible: bool,
    pub(crate) live_resize: bool,
    pub(crate) viewport_total_rows: u64,
    pub(crate) viewport_visible_rows: u64,
    pub(crate) viewport_offset_rows: u64,
    pub(crate) selection_present: bool,
    pub(crate) resize_requests: u64,
    pub(crate) resize_notifications: u64,
    pub(crate) resize_applied: u64,
    pub(crate) resize_coalesced: u64,
    pub(crate) pty_rows: u64,
    pub(crate) pty_columns: u64,
    pub(crate) pty_pixel_width: u64,
    pub(crate) pty_pixel_height: u64,
    pub(crate) terminal_inputs_accepted: u64,
    pub(crate) lifecycle: RuntimeLifecycle,
    pub(crate) observer_drops: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeVisibility {
    pub(crate) presentable: bool,
    pub(crate) minimized: bool,
    pub(crate) occluded: bool,
    pub(crate) workspace_visible: bool,
    pub(crate) pane_visible: bool,
    pub(crate) live_resize: bool,
}

impl RuntimeObservation {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(RuntimeObservationState {
                worker_generation: AtomicU64::new(0),
                screens_published: AtomicU64::new(0),
                screens_enqueued: AtomicU64::new(0),
                screens_superseded: AtomicU64::new(0),
                event_queue_length: AtomicU64::new(0),
                event_queue_high_water: AtomicU64::new(0),
                ui_dispatches: AtomicU64::new(0),
                ui_screen_events: AtomicU64::new(0),
                ui_drain_high_water: AtomicU64::new(0),
                ui_latest_generation: AtomicU64::new(0),
                render_latest_generation: AtomicU64::new(0),
                next_frame_generation: AtomicU64::new(0),
                next_frame_count: AtomicU64::new(0),
                presentable: AtomicBool::new(false),
                minimized: AtomicBool::new(false),
                occluded: AtomicBool::new(true),
                workspace_visible: AtomicBool::new(false),
                pane_visible: AtomicBool::new(false),
                live_resize: AtomicBool::new(false),
                visibility_version: AtomicU64::new(0),
                viewport_version: AtomicU64::new(0),
                viewport_total_rows: AtomicU64::new(0),
                viewport_visible_rows: AtomicU64::new(0),
                viewport_offset_rows: AtomicU64::new(0),
                selection_present: AtomicBool::new(false),
                resize_requests: AtomicU64::new(0),
                resize_notifications: AtomicU64::new(0),
                resize_applied: AtomicU64::new(0),
                resize_coalesced: AtomicU64::new(0),
                pty_geometry_version: AtomicU64::new(0),
                pty_rows: AtomicU64::new(0),
                pty_columns: AtomicU64::new(0),
                pty_pixel_width: AtomicU64::new(0),
                pty_pixel_height: AtomicU64::new(0),
                terminal_inputs_accepted: AtomicU64::new(0),
                lifecycle: AtomicU8::new(RuntimeLifecycle::Starting as u8),
                observer_drops: AtomicU64::new(0),
                observer_failed: AtomicBool::new(false),
                sealed: AtomicBool::new(false),
                ui_attached: AtomicBool::new(true),
                presentable_known: AtomicU8::new(2),
                restore_pending_generation: AtomicU64::new(0),
                next_event_sequence: AtomicU64::new(0),
                transitions: Mutex::new(VecDeque::with_capacity(RUNTIME_TRANSITION_CAPACITY)),
            }),
        }
    }

    pub(crate) fn worker_started(&self, geometry: TerminalGeometry) {
        self.store_pty_geometry(geometry);
        self.state
            .lifecycle
            .store(RuntimeLifecycle::Running as u8, Ordering::Release);
    }

    pub(crate) fn screen_published(&self, generation: u64) {
        if self
            .state
            .worker_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (generation > current).then_some(generation)
            })
            .is_ok()
        {
            self.increment(&self.state.screens_published);
        } else {
            self.mark_failed();
        }
    }

    pub(crate) fn screen_enqueued(
        &self,
        queue_length: usize,
        evicted_event: bool,
        superseded_screen: bool,
    ) {
        self.increment(&self.state.screens_enqueued);
        if superseded_screen {
            self.increment(&self.state.screens_superseded);
        }
        self.observe_event_queue_length(queue_length, evicted_event);
    }

    pub(crate) fn event_enqueued(
        &self,
        queue_length: usize,
        evicted_event: bool,
        superseded_screen: bool,
    ) {
        if superseded_screen {
            self.increment(&self.state.screens_superseded);
        }
        self.observe_event_queue_length(queue_length, evicted_event);
    }

    pub(crate) fn event_send_failed(&self) {
        if self.state.ui_attached.load(Ordering::Acquire) {
            self.mark_failed();
        }
    }

    pub(crate) fn ui_dispatch(&self, drain_count: usize, queue_length: usize) {
        self.increment(&self.state.ui_dispatches);
        self.update_high_water(&self.state.ui_drain_high_water, drain_count as u64);
        self.state
            .event_queue_length
            .store(queue_length as u64, Ordering::Relaxed);
    }

    pub(crate) fn ui_screen_received(&self) {
        self.increment(&self.state.ui_screen_events);
    }

    pub(crate) fn ui_screen_applied(
        &self,
        generation: u64,
        total_rows: u64,
        visible_rows: u64,
        offset_rows: u64,
        selection_present: bool,
    ) {
        self.store_nondecreasing(&self.state.ui_latest_generation, generation);
        self.store_viewport(total_rows, visible_rows, offset_rows, selection_present);
    }

    pub(crate) fn render_started(&self, generation: u64) {
        self.store_nondecreasing(&self.state.render_latest_generation, generation);
    }

    pub(crate) fn next_frame(&self, generation: u64) {
        if !self.state.presentable.load(Ordering::Acquire) {
            return;
        }
        self.store_nondecreasing(&self.state.next_frame_generation, generation);
        self.increment(&self.state.next_frame_count);
        let restored = self
            .state
            .restore_pending_generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |pending| {
                (pending != 0 && generation >= pending - 1).then_some(0)
            })
            .is_ok();
        if restored {
            self.push_transition(
                RuntimeEventKind::FirstNextFrameAfterRestore,
                generation,
                0,
                0,
            );
        }
    }

    pub(crate) fn visibility(&self, visibility: RuntimeVisibility) {
        let Some(base_version) = self.begin_fact_update(&self.state.visibility_version) else {
            return;
        };
        self.state
            .minimized
            .store(visibility.minimized, Ordering::Relaxed);
        self.state
            .occluded
            .store(visibility.occluded, Ordering::Relaxed);
        self.state
            .workspace_visible
            .store(visibility.workspace_visible, Ordering::Relaxed);
        self.state
            .pane_visible
            .store(visibility.pane_visible, Ordering::Relaxed);
        self.state
            .live_resize
            .store(visibility.live_resize, Ordering::Relaxed);
        self.state
            .presentable
            .store(visibility.presentable, Ordering::Relaxed);
        self.state
            .visibility_version
            .store(base_version + 2, Ordering::Release);

        let encoded = u8::from(visibility.presentable);
        let previous = self.state.presentable_known.swap(encoded, Ordering::AcqRel);
        if previous == 2 || previous == encoded {
            return;
        }
        let generation = self
            .state
            .render_latest_generation
            .load(Ordering::Acquire)
            .max(self.state.ui_latest_generation.load(Ordering::Acquire));
        if visibility.presentable {
            let pending = generation.saturating_add(1);
            self.state
                .restore_pending_generation
                .store(pending, Ordering::Release);
            self.push_transition(RuntimeEventKind::VisibilityRestored, generation, 0, 0);
        } else {
            self.state
                .restore_pending_generation
                .store(0, Ordering::Release);
            self.push_transition(RuntimeEventKind::VisibilityLost, generation, 0, 0);
        }
    }

    pub(crate) fn product_visibility(&self, workspace_visible: bool, pane_visible: bool) {
        let visibility = RuntimeVisibility {
            presentable: !self.state.minimized.load(Ordering::Relaxed)
                && !self.state.occluded.load(Ordering::Relaxed)
                && workspace_visible
                && pane_visible,
            minimized: self.state.minimized.load(Ordering::Relaxed),
            occluded: self.state.occluded.load(Ordering::Relaxed),
            workspace_visible,
            pane_visible,
            live_resize: self.state.live_resize.load(Ordering::Relaxed),
        };
        self.visibility(visibility);
    }

    pub(crate) fn resize_requested(&self, notified: bool, coalesced: bool) {
        self.increment(&self.state.resize_requests);
        if notified {
            self.increment(&self.state.resize_notifications);
        }
        if coalesced {
            self.increment(&self.state.resize_coalesced);
        }
    }

    pub(crate) fn resize_applied(&self, geometry: TerminalGeometry) {
        self.store_pty_geometry(geometry);
        self.increment(&self.state.resize_applied);
    }

    pub(crate) fn terminal_input_accepted(&self) {
        self.increment(&self.state.terminal_inputs_accepted);
    }

    pub(crate) fn pane_released(&self) {
        self.state.ui_attached.store(false, Ordering::Release);
        self.visibility(RuntimeVisibility {
            presentable: false,
            minimized: self.state.minimized.load(Ordering::Relaxed),
            occluded: self.state.occluded.load(Ordering::Relaxed),
            workspace_visible: self.state.workspace_visible.load(Ordering::Relaxed),
            pane_visible: false,
            live_resize: self.state.live_resize.load(Ordering::Relaxed),
        });
    }

    pub(crate) fn session_exited(&self, class_code: u64) {
        self.push_terminal_transition(
            RuntimeLifecycle::Exited,
            RuntimeEventKind::SessionExited,
            class_code,
        );
    }

    pub(crate) fn session_failed(&self, class_code: u64) {
        self.push_terminal_transition(
            RuntimeLifecycle::Failed,
            RuntimeEventKind::SessionFailed,
            class_code,
        );
    }

    pub(crate) fn sample(&self) -> RuntimeSample {
        let (pty_rows, pty_columns, pty_pixel_width, pty_pixel_height) = self.load_pty_geometry();
        let (viewport_total_rows, viewport_visible_rows, viewport_offset_rows, selection_present) =
            self.load_viewport();
        let visibility = self.load_visibility();
        // Load related monotonic facts in consumer-to-producer order. Each producer publishes its
        // earlier fact before its later one, so a sample cannot manufacture an impossible lead
        // while the producer advances between two loads.
        let next_frame_generation = self.state.next_frame_generation.load(Ordering::Acquire);
        let render_latest_generation = self.state.render_latest_generation.load(Ordering::Acquire);
        let ui_latest_generation = self.state.ui_latest_generation.load(Ordering::Acquire);
        let worker_generation = self.state.worker_generation.load(Ordering::Acquire);
        let screens_superseded = self.state.screens_superseded.load(Ordering::Relaxed);
        let ui_screen_events = self.state.ui_screen_events.load(Ordering::Relaxed);
        let screens_enqueued = self.state.screens_enqueued.load(Ordering::Relaxed);
        let screens_published = self.state.screens_published.load(Ordering::Relaxed);
        let resize_applied = self.state.resize_applied.load(Ordering::Relaxed);
        let resize_notifications = self.state.resize_notifications.load(Ordering::Relaxed);
        let resize_coalesced = self.state.resize_coalesced.load(Ordering::Relaxed);
        let resize_requests = self.state.resize_requests.load(Ordering::Relaxed);
        RuntimeSample {
            continuous_ns: continuous_time_ns().unwrap_or_else(|| {
                self.mark_failed();
                0
            }),
            worker_generation,
            screens_published,
            screens_enqueued,
            screens_superseded,
            event_queue_length: self.state.event_queue_length.load(Ordering::Relaxed),
            event_queue_high_water: self.state.event_queue_high_water.load(Ordering::Relaxed),
            ui_dispatches: self.state.ui_dispatches.load(Ordering::Relaxed),
            ui_screen_events,
            ui_drain_high_water: self.state.ui_drain_high_water.load(Ordering::Relaxed),
            ui_latest_generation,
            render_latest_generation,
            next_frame_generation,
            next_frame_count: self.state.next_frame_count.load(Ordering::Relaxed),
            presentable: visibility.presentable,
            minimized: visibility.minimized,
            occluded: visibility.occluded,
            workspace_visible: visibility.workspace_visible,
            pane_visible: visibility.pane_visible,
            live_resize: visibility.live_resize,
            viewport_total_rows,
            viewport_visible_rows,
            viewport_offset_rows,
            selection_present,
            resize_requests,
            resize_notifications,
            resize_applied,
            resize_coalesced,
            pty_rows,
            pty_columns,
            pty_pixel_width,
            pty_pixel_height,
            terminal_inputs_accepted: self.state.terminal_inputs_accepted.load(Ordering::Relaxed),
            lifecycle: RuntimeLifecycle::from_u8(self.state.lifecycle.load(Ordering::Acquire)),
            observer_drops: self.state.observer_drops.load(Ordering::Acquire),
        }
    }

    pub(crate) fn drain_transitions(&self) -> Vec<RuntimeTransition> {
        let Ok(mut transitions) = self.state.transitions.lock() else {
            self.mark_failed();
            return Vec::new();
        };
        transitions.drain(..).collect()
    }

    pub(crate) fn seal_and_drain_transitions(&self) -> Vec<RuntimeTransition> {
        let Ok(mut transitions) = self.state.transitions.lock() else {
            self.mark_failed();
            return Vec::new();
        };
        self.state.sealed.store(true, Ordering::Release);
        transitions.drain(..).collect()
    }

    pub(crate) fn is_failed(&self) -> bool {
        self.state.observer_failed.load(Ordering::Acquire)
    }

    pub(crate) fn fail(&self) {
        self.mark_failed();
    }

    fn observe_event_queue_length(&self, queue_length: usize, evicted_event: bool) {
        let queue_length = queue_length as u64;
        let occupied_at_send = if evicted_event { 2 } else { 1 };
        self.update_high_water(
            &self.state.event_queue_high_water,
            queue_length.max(occupied_at_send),
        );
        self.state
            .event_queue_length
            .store(queue_length, Ordering::Relaxed);
    }

    fn store_viewport(
        &self,
        total_rows: u64,
        visible_rows: u64,
        offset_rows: u64,
        selection_present: bool,
    ) {
        let Some(base_version) = self.begin_fact_update(&self.state.viewport_version) else {
            return;
        };
        self.state
            .viewport_total_rows
            .store(total_rows, Ordering::Relaxed);
        self.state
            .viewport_visible_rows
            .store(visible_rows, Ordering::Relaxed);
        self.state
            .viewport_offset_rows
            .store(offset_rows, Ordering::Relaxed);
        self.state
            .selection_present
            .store(selection_present, Ordering::Relaxed);
        self.state
            .viewport_version
            .store(base_version + 2, Ordering::Release);
    }

    fn load_visibility(&self) -> RuntimeVisibility {
        for _ in 0..1024 {
            let before = self.state.visibility_version.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::thread::yield_now();
                continue;
            }
            let visibility = RuntimeVisibility {
                presentable: self.state.presentable.load(Ordering::Relaxed),
                minimized: self.state.minimized.load(Ordering::Relaxed),
                occluded: self.state.occluded.load(Ordering::Relaxed),
                workspace_visible: self.state.workspace_visible.load(Ordering::Relaxed),
                pane_visible: self.state.pane_visible.load(Ordering::Relaxed),
                live_resize: self.state.live_resize.load(Ordering::Relaxed),
            };
            if before == self.state.visibility_version.load(Ordering::Acquire) {
                return visibility;
            }
        }
        self.mark_failed();
        RuntimeVisibility {
            presentable: false,
            minimized: true,
            occluded: true,
            workspace_visible: false,
            pane_visible: false,
            live_resize: false,
        }
    }

    fn load_viewport(&self) -> (u64, u64, u64, bool) {
        for _ in 0..1024 {
            let before = self.state.viewport_version.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::thread::yield_now();
                continue;
            }
            let result = (
                self.state.viewport_total_rows.load(Ordering::Relaxed),
                self.state.viewport_visible_rows.load(Ordering::Relaxed),
                self.state.viewport_offset_rows.load(Ordering::Relaxed),
                self.state.selection_present.load(Ordering::Relaxed),
            );
            if before == self.state.viewport_version.load(Ordering::Acquire) {
                return result;
            }
        }
        self.mark_failed();
        (
            self.state.viewport_total_rows.load(Ordering::Relaxed),
            self.state.viewport_visible_rows.load(Ordering::Relaxed),
            self.state.viewport_offset_rows.load(Ordering::Relaxed),
            self.state.selection_present.load(Ordering::Relaxed),
        )
    }

    fn store_pty_geometry(&self, geometry: TerminalGeometry) {
        let Some(base_version) = self.begin_fact_update(&self.state.pty_geometry_version) else {
            return;
        };
        let grid = geometry.grid();
        let backing = geometry.backing_grid_size();
        self.state
            .pty_rows
            .store(u64::from(grid.rows), Ordering::Relaxed);
        self.state
            .pty_columns
            .store(u64::from(grid.cols), Ordering::Relaxed);
        self.state
            .pty_pixel_width
            .store(u64::from(backing.width), Ordering::Relaxed);
        self.state
            .pty_pixel_height
            .store(u64::from(backing.height), Ordering::Relaxed);
        self.state
            .pty_geometry_version
            .store(base_version + 2, Ordering::Release);
    }

    fn begin_fact_update(&self, version: &AtomicU64) -> Option<u64> {
        let updated = version
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                (value & 1 == 0 && value <= u64::MAX - 2).then_some(value + 1)
            })
            .ok();
        if updated.is_none() {
            self.mark_failed();
        }
        updated
    }

    fn load_pty_geometry(&self) -> (u64, u64, u64, u64) {
        for _ in 0..1024 {
            let before = self.state.pty_geometry_version.load(Ordering::Acquire);
            if before & 1 != 0 {
                std::thread::yield_now();
                continue;
            }
            let result = (
                self.state.pty_rows.load(Ordering::Relaxed),
                self.state.pty_columns.load(Ordering::Relaxed),
                self.state.pty_pixel_width.load(Ordering::Relaxed),
                self.state.pty_pixel_height.load(Ordering::Relaxed),
            );
            let after = self.state.pty_geometry_version.load(Ordering::Acquire);
            if before == after {
                return result;
            }
        }
        self.mark_failed();
        (
            self.state.pty_rows.load(Ordering::Relaxed),
            self.state.pty_columns.load(Ordering::Relaxed),
            self.state.pty_pixel_width.load(Ordering::Relaxed),
            self.state.pty_pixel_height.load(Ordering::Relaxed),
        )
    }

    fn push_transition(&self, kind: RuntimeEventKind, generation: u64, aux0: u64, aux1: u64) {
        let Ok(mut transitions) = self.state.transitions.lock() else {
            self.mark_failed();
            return;
        };
        if self.state.sealed.load(Ordering::Acquire) {
            drop(transitions);
            self.mark_failed();
            return;
        }
        if transitions.len() == RUNTIME_TRANSITION_CAPACITY {
            drop(transitions);
            self.increment_drop();
            self.mark_failed();
            return;
        }
        let Ok(sequence) = self.state.next_event_sequence.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |value| value.checked_add(1),
        ) else {
            drop(transitions);
            self.mark_failed();
            return;
        };
        let Some(continuous_ns) = continuous_time_ns() else {
            drop(transitions);
            self.mark_failed();
            return;
        };
        transitions.push_back(RuntimeTransition {
            sequence,
            continuous_ns,
            kind,
            generation,
            aux0,
            aux1,
        });
    }

    fn push_terminal_transition(
        &self,
        lifecycle: RuntimeLifecycle,
        kind: RuntimeEventKind,
        class_code: u64,
    ) {
        if self.is_failed() {
            return;
        }
        let Ok(mut transitions) = self.state.transitions.lock() else {
            self.mark_failed();
            return;
        };
        let current = self.state.lifecycle.load(Ordering::Acquire);
        if current == RuntimeLifecycle::Exited as u8 || current == RuntimeLifecycle::Failed as u8 {
            return;
        }
        if self.state.sealed.load(Ordering::Acquire) {
            drop(transitions);
            self.mark_failed();
            return;
        }
        if transitions.len() == RUNTIME_TRANSITION_CAPACITY {
            drop(transitions);
            self.increment_drop();
            self.mark_failed();
            return;
        }
        let Ok(sequence) = self.state.next_event_sequence.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |value| value.checked_add(1),
        ) else {
            drop(transitions);
            self.mark_failed();
            return;
        };
        let Some(continuous_ns) = continuous_time_ns() else {
            drop(transitions);
            self.mark_failed();
            return;
        };
        transitions.push_back(RuntimeTransition {
            sequence,
            continuous_ns,
            kind,
            generation: self.state.worker_generation.load(Ordering::Acquire),
            aux0: class_code,
            aux1: 0,
        });
        self.state
            .lifecycle
            .store(lifecycle as u8, Ordering::Release);
    }

    fn increment(&self, value: &AtomicU64) {
        if value
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .is_err()
        {
            self.mark_failed();
        }
    }

    fn increment_drop(&self) {
        if self
            .state
            .observer_drops
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .is_err()
        {
            self.state.observer_drops.store(u64::MAX, Ordering::Release);
        }
    }

    fn update_high_water(&self, value: &AtomicU64, observed: u64) {
        let mut current = value.load(Ordering::Relaxed);
        while observed > current {
            match value.compare_exchange_weak(
                current,
                observed,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(updated) => current = updated,
            }
        }
    }

    fn store_nondecreasing(&self, value: &AtomicU64, observed: u64) {
        let mut current = value.load(Ordering::Acquire);
        if observed < current {
            self.mark_failed();
            return;
        }
        while observed > current {
            match value.compare_exchange_weak(
                current,
                observed,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(updated) => {
                    if observed < updated {
                        self.mark_failed();
                        return;
                    }
                    current = updated;
                }
            }
        }
    }

    fn mark_failed(&self) {
        if !self.state.observer_failed.swap(true, Ordering::AcqRel) {
            self.state
                .lifecycle
                .store(RuntimeLifecycle::ObserverFailed as u8, Ordering::Release);
            self.push_observer_failure_transition();
        }
    }

    fn push_observer_failure_transition(&self) {
        let Ok(mut transitions) = self.state.transitions.lock() else {
            return;
        };
        if self.state.sealed.load(Ordering::Acquire) {
            return;
        }
        if transitions.len() == RUNTIME_TRANSITION_CAPACITY {
            return;
        }
        let Ok(sequence) = self.state.next_event_sequence.fetch_update(
            Ordering::AcqRel,
            Ordering::Acquire,
            |value| value.checked_add(1),
        ) else {
            return;
        };
        let Some(continuous_ns) = continuous_time_ns() else {
            return;
        };
        transitions.push_back(RuntimeTransition {
            sequence,
            continuous_ns,
            kind: RuntimeEventKind::ObserverFailed,
            generation: self.state.worker_generation.load(Ordering::Acquire),
            aux0: 0,
            aux1: 0,
        });
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_continuous_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

fn continuous_time_ns() -> Option<u64> {
    static TIMEBASE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    let &(numer, denom) = TIMEBASE
        .get_or_init(|| {
            let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
            // SAFETY: `info` is writable for the duration of this synchronous system call.
            let status = unsafe { mach_timebase_info(&raw mut info) };
            (status == 0 && info.numer != 0 && info.denom != 0).then_some((info.numer, info.denom))
        })
        .as_ref()?;
    // SAFETY: `mach_continuous_time` has no arguments and is available on the supported macOS.
    let ticks = unsafe { mach_continuous_time() };
    let nanoseconds = u128::from(ticks)
        .checked_mul(u128::from(numer))?
        .checked_div(u128::from(denom))?;
    u64::try_from(nanoseconds).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_queue_is_fifo_bounded_and_fails_closed() {
        let observation = RuntimeObservation::new();
        for generation in 0..RUNTIME_TRANSITION_CAPACITY as u64 {
            observation.push_transition(RuntimeEventKind::VisibilityLost, generation, 0, 0);
        }
        observation.push_transition(RuntimeEventKind::VisibilityRestored, 65, 0, 0);

        let transitions = observation.drain_transitions();
        assert_eq!(transitions.len(), RUNTIME_TRANSITION_CAPACITY);
        assert_eq!(transitions.first().map(|event| event.sequence), Some(0));
        assert_eq!(transitions.last().map(|event| event.sequence), Some(63));
        assert!(observation.is_failed());
        assert_ne!(observation.sample().observer_drops, 0);
    }

    #[test]
    fn restore_records_only_the_first_matching_next_frame() {
        let observation = RuntimeObservation::new();
        observation.visibility(RuntimeVisibility {
            presentable: false,
            minimized: false,
            occluded: true,
            workspace_visible: true,
            pane_visible: true,
            live_resize: false,
        });
        observation.render_started(7);
        observation.visibility(RuntimeVisibility {
            presentable: true,
            minimized: false,
            occluded: false,
            workspace_visible: true,
            pane_visible: true,
            live_resize: false,
        });
        observation.next_frame(6);
        observation.next_frame(7);
        observation.next_frame(8);

        let transitions = observation.drain_transitions();
        assert_eq!(
            transitions
                .iter()
                .filter(|event| { event.kind == RuntimeEventKind::FirstNextFrameAfterRestore })
                .count(),
            1
        );
    }

    #[test]
    fn terminal_lifecycle_is_ordered_and_recorded_once() {
        let observation = RuntimeObservation::new();
        observation.worker_started(TerminalGeometry::from_grid(
            super::super::geometry::CellGridSize::new(80, 24),
            super::super::geometry::LogicalCellSize::new(10.0, 20.0),
            super::super::geometry::BackingScale::new(2.0).unwrap(),
        ));
        observation.session_exited(4);
        observation.session_exited(4);
        observation.session_failed(5);

        assert_eq!(observation.sample().lifecycle, RuntimeLifecycle::Exited);
        let transitions = observation.drain_transitions();
        assert_eq!(
            transitions
                .iter()
                .filter(|event| event.kind == RuntimeEventKind::SessionExited)
                .count(),
            1
        );
        assert!(
            transitions
                .iter()
                .all(|event| event.kind != RuntimeEventKind::SessionFailed)
        );
    }

    #[test]
    fn checked_counter_overflow_fails_the_observation() {
        let observation = RuntimeObservation::new();
        observation
            .state
            .screens_published
            .store(u64::MAX, Ordering::Relaxed);

        observation.screen_published(1);

        assert!(observation.is_failed());
        assert_eq!(
            observation.sample().lifecycle,
            RuntimeLifecycle::ObserverFailed
        );
    }

    #[test]
    fn repeated_presentation_generation_fails_closed() {
        let observation = RuntimeObservation::new();
        observation.screen_published(1);
        observation.screen_published(1);

        assert!(observation.is_failed());
        assert_eq!(observation.sample().screens_published, 1);
    }
}
