use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Edges, ElementId, HitboxBehavior, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, Point, RenderOnce, SharedString, Styled as _, Window, canvas, div, px,
};

const DEFAULT_DRAG_THRESHOLD: f32 = 4.0;
const MINIMUM_DRAG_THRESHOLD: f32 = 1.0;
const MAXIMUM_DRAG_THRESHOLD: f32 = 32.0;
const MAXIMUM_POINTER_INSET: f32 = 4096.0;

/// Stable identity for one primary-pointer interaction owned by a [`WindowDragRegion`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct WindowDragInteractionId(u64);

impl WindowDragInteractionId {
    /// Returns the monotonic numeric identity.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A read-only application handle to the interaction currently owned by a drag region.
///
/// The control remains the sole mutator. Applications may retain a clone to derive Terminal Input
/// Focus or other policy without duplicating the pointer lifecycle in application state.
#[derive(Clone, Debug, Default)]
pub struct WindowDragRegionStatus {
    active_interaction: Rc<Cell<Option<WindowDragInteractionId>>>,
}

impl WindowDragRegionStatus {
    /// Creates an idle status handle.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the currently owned interaction, if any.
    pub fn active_interaction(&self) -> Option<WindowDragInteractionId> {
        self.active_interaction.get()
    }

    /// Returns whether the region currently owns a primary-pointer interaction.
    pub fn is_active(&self) -> bool {
        self.active_interaction().is_some()
    }

    fn begin(&self, interaction: WindowDragInteractionId) {
        self.active_interaction.set(Some(interaction));
    }

    fn finish(&self, interaction: WindowDragInteractionId) {
        if self.active_interaction() == Some(interaction) {
            self.active_interaction.set(None);
        }
    }
}

/// Why a [`WindowDragRegion`] released an owned primary-pointer interaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowDragFinishReason {
    /// The primary pointer button was released.
    Completed,
    /// Pointer motion reported that the primary button was no longer held.
    PointerButtonLost,
    /// The pointer left the Operating-System Window during the interaction.
    PointerExited,
    /// The application handed movement to the Operating-System Window system.
    OperatingSystemWindowMoveStarted,
    /// The Operating-System Window became inactive.
    WindowDeactivated,
    /// The region became disabled while it owned the interaction.
    Disabled,
    /// The keyed region state was released while it owned the interaction.
    RegionRemoved,
}

impl WindowDragFinishReason {
    fn suppresses_pointer_until_release(self) -> bool {
        !matches!(self, Self::Completed | Self::PointerButtonLost)
    }
}

/// A policy-neutral request emitted by [`WindowDragRegion`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowDragRegionEvent {
    /// The region took ownership of a primary-pointer press.
    InteractionStarted {
        /// Stable identity for this interaction.
        interaction: WindowDragInteractionId,
    },
    /// Movement crossed the configured logical threshold.
    ///
    /// This request is emitted exactly once for an owned press. The application decides how to
    /// adapt it into an Operating-System Window move.
    MoveRequested {
        /// Stable identity for this interaction.
        interaction: WindowDragInteractionId,
    },
    /// A primary-pointer double activation occurred without starting a drag interaction.
    DoubleActivationRequested,
    /// The region released ownership of a primary-pointer interaction.
    InteractionFinished {
        /// Stable identity for this interaction.
        interaction: WindowDragInteractionId,
        /// Why ownership ended.
        reason: WindowDragFinishReason,
    },
}

/// How the application handled a [`WindowDragRegionEvent`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WindowDragRegionResponse {
    /// The control continues to own the pointer interaction.
    #[default]
    Continue,
    /// The Operating-System Window system accepted the move interaction.
    ///
    /// The control ends its active interaction immediately because a native handoff may consume the
    /// eventual pointer release. Stray motion and release remain suppressed until release or the
    /// next clean primary press.
    OperatingSystemWindowMoveStarted,
}

impl From<()> for WindowDragRegionResponse {
    fn from((): ()) -> Self {
        Self::Continue
    }
}

type WindowDragHandler =
    Rc<dyn Fn(&WindowDragRegionEvent, &mut Window, &mut App) -> WindowDragRegionResponse>;

/// A platform-neutral interaction region for requesting an Operating-System Window move.
///
/// The control owns primary-button eligibility, pointer capture, logical-coordinate threshold
/// detection, exactly-once move requests, double activation, cancellation, and propagation for an
/// owned gesture. The application owns top-chrome layout and paint, terminal focus coordination,
/// actual Operating-System Window movement, and double-activation policy.
///
/// The pointer hitbox is painted below `content`, may be inset to reserve neighboring controls,
/// and claims primary down in the bubble phase only after frontmost capture handlers have had an
/// opportunity to consume it. Interactive
/// children follow the normal GPUI contract of stopping handled events or installing a blocking
/// hitbox; SpaceTerm Buttons, Menu triggers, selectors, and Resize Handles do so. Occluding and
/// capture-owning overlays therefore retain their behavior, while uncovered space remains
/// draggable. Once the region owns a press, its move and release events are captured even outside
/// the original bounds and do not propagate to parent or terminal pointer handlers.
///
/// Events are ordered as `InteractionStarted`, an optional `MoveRequested`, then
/// `InteractionFinished`. A double activation emits only `DoubleActivationRequested`. The drag
/// threshold is measured in GPUI logical coordinates and defaults to a bounded compact-desktop
/// value of four logical pixels.
///
/// A logical accessibility name is mandatory. GPUI 0.2.2 cannot yet publish a custom drag-region
/// role to the native accessibility tree, but retaining the name in the public interface keeps the
/// semantic contract explicit and provides the default debug selector.
#[derive(IntoElement)]
pub struct WindowDragRegion {
    id: ElementId,
    accessibility_name: SharedString,
    content: AnyElement,
    status: WindowDragRegionStatus,
    disabled: bool,
    drag_threshold: Pixels,
    pointer_insets: Edges<Pixels>,
    debug_selector: Option<String>,
    on_event: Option<WindowDragHandler>,
}

impl WindowDragRegion {
    /// Creates a region with stable identity, a mandatory logical name, and caller-owned content.
    ///
    /// The region fills its containing bounds and adds no paint or layout metrics of its own.
    /// Pointer insets affect only its internal interaction target.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        content: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            content: content.into_any_element(),
            status: WindowDragRegionStatus::new(),
            disabled: false,
            drag_threshold: px(DEFAULT_DRAG_THRESHOLD),
            pointer_insets: Edges::default(),
            debug_selector: None,
            on_event: None,
        }
    }

    /// Connects a retained read-only status handle for application policy derivation.
    pub fn status(mut self, status: WindowDragRegionStatus) -> Self {
        self.status = status;
        self
    }

    /// Controls whether the region may own new pointer interactions.
    ///
    /// Disabling an active region cancels its owned interaction.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Sets the logical movement threshold before a move is requested.
    ///
    /// Finite values are clamped to the desktop range from one through thirty-two logical pixels.
    /// Non-finite values use the four-pixel default.
    pub fn drag_threshold(mut self, threshold: Pixels) -> Self {
        self.drag_threshold = bounded_drag_threshold(threshold);
        self
    }

    /// Insets the draggable pointer target without changing content layout or paint.
    ///
    /// Applications can reserve a neighboring control's pointer corridor so the two controls do
    /// not depend on event-ordering precedence for exclusive gesture ownership.
    pub fn pointer_insets(mut self, insets: Edges<Pixels>) -> Self {
        self.pointer_insets = Edges {
            top: bounded_pointer_inset(insets.top),
            right: bounded_pointer_inset(insets.right),
            bottom: bounded_pointer_inset(insets.bottom),
            left: bounded_pointer_inset(insets.left),
        };
        self
    }

    /// Adds a stable root debug selector.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Handles ordered interaction and policy-neutral move requests.
    pub fn on_event<R>(
        mut self,
        handler: impl Fn(&WindowDragRegionEvent, &mut Window, &mut App) -> R + 'static,
    ) -> Self
    where
        R: Into<WindowDragRegionResponse>,
    {
        self.on_event = Some(Rc::new(move |event, window, cx| {
            handler(event, window, cx).into()
        }));
        self
    }
}

impl RenderOnce for WindowDragRegion {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let enabled = !self.disabled && self.on_event.is_some();
        let state = window.use_keyed_state(self.id.clone(), cx, WindowDragRegionState::new);
        let cancelled = state.update(cx, |state, _| {
            state.synchronize(
                enabled,
                self.drag_threshold,
                self.status,
                self.on_event.clone(),
            )
        });
        schedule_events(self.on_event.clone(), cancelled, window, cx);

        let down_state = state.clone();
        let move_state = state.clone();
        let up_state = state.clone();
        let exit_state = state;
        let down_handler = self.on_event.clone();
        let move_handler = self.on_event.clone();
        let up_handler = self.on_event.clone();
        let exit_handler = self.on_event.clone();
        let pointer_tracker = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let down_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.bubble()
                        || event.button != MouseButton::Left
                        || !down_hitbox.is_hovered(window)
                        || !down_state.read(cx).enabled
                    {
                        return;
                    }

                    let events = match event.click_count {
                        1 => down_state.update(cx, |state, _| state.pointer_down(event.position)),
                        2 => down_state.update(cx, |state, _| state.double_activation()),
                        _ => {
                            down_state.update(cx, |state, _| state.suppress_click_release());
                            Vec::new()
                        }
                    };
                    window.prevent_default();
                    emit_events(down_handler.clone(), events, window, cx);
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.capture() || !move_state.read(cx).owns_pointer_stream() {
                        return;
                    }
                    let events = move_state.update(cx, |state, _| {
                        state.pointer_move(event.position, event.pressed_button)
                    });
                    window.prevent_default();
                    let response = emit_events(move_handler.clone(), events, window, cx);
                    if response == WindowDragRegionResponse::OperatingSystemWindowMoveStarted {
                        let events = move_state.update(cx, |state, _| {
                            state.finish(WindowDragFinishReason::OperatingSystemWindowMoveStarted)
                        });
                        emit_events(move_handler.clone(), events, window, cx);
                    }
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || !up_state.read(cx).owns_pointer_stream()
                    {
                        return;
                    }
                    let events = up_state.update(cx, |state, _| state.pointer_up());
                    window.prevent_default();
                    emit_events(up_handler.clone(), events, window, cx);
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                    if !phase.capture() || !exit_state.read(cx).owns_pointer_stream() {
                        return;
                    }
                    let events = exit_state.update(cx, |state, _| {
                        state.finish(WindowDragFinishReason::PointerExited)
                    });
                    window.prevent_default();
                    emit_events(exit_handler.clone(), events, window, cx);
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();

        let root_selector = self
            .debug_selector
            .unwrap_or_else(|| self.accessibility_name.to_string());
        let hitbox_selector = format!("{root_selector}-hitbox");
        let pointer_insets = self.pointer_insets;
        let pointer_target = div()
            .id("window-drag-region-hitbox")
            .debug_selector(move || hitbox_selector)
            .absolute()
            .top(pointer_insets.top)
            .right(pointer_insets.right)
            .bottom(pointer_insets.bottom)
            .left(pointer_insets.left)
            .child(pointer_tracker);
        div()
            .id(self.id)
            .debug_selector(move || root_selector)
            .relative()
            .size_full()
            .cursor_default()
            .child(pointer_target)
            .child(self.content)
    }
}

fn bounded_pointer_inset(inset: Pixels) -> Pixels {
    let value = f32::from(inset);
    px(if value.is_finite() {
        value.clamp(0.0, MAXIMUM_POINTER_INSET)
    } else {
        0.0
    })
}

fn bounded_drag_threshold(threshold: Pixels) -> Pixels {
    let value = f32::from(threshold);
    px(if value.is_finite() {
        value.clamp(MINIMUM_DRAG_THRESHOLD, MAXIMUM_DRAG_THRESHOLD)
    } else {
        DEFAULT_DRAG_THRESHOLD
    })
}

fn emit_events(
    handler: Option<WindowDragHandler>,
    events: Vec<WindowDragRegionEvent>,
    window: &mut Window,
    cx: &mut App,
) -> WindowDragRegionResponse {
    let Some(handler) = handler else {
        return WindowDragRegionResponse::Continue;
    };
    let mut move_response = WindowDragRegionResponse::Continue;
    for event in events {
        let response = handler(&event, window, cx);
        if matches!(event, WindowDragRegionEvent::MoveRequested { .. }) {
            move_response = response;
        }
    }
    move_response
}

fn schedule_events(
    handler: Option<WindowDragHandler>,
    events: Vec<WindowDragRegionEvent>,
    window: &mut Window,
    cx: &mut App,
) {
    if events.is_empty() || handler.is_none() {
        return;
    }
    window.defer(cx, move |window, cx| {
        let _ = emit_events(handler, events, window, cx);
    });
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OwnedPress {
    interaction: WindowDragInteractionId,
    origin: Point<Pixels>,
    current: Point<Pixels>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum PointerLifecycle {
    Idle,
    Pressed(OwnedPress),
    MoveRequested(OwnedPress),
    CancelledUntilRelease,
}

struct WindowDragRegionState {
    enabled: bool,
    drag_threshold: Pixels,
    status: WindowDragRegionStatus,
    pointer: PointerLifecycle,
    next_interaction_id: u64,
    handler: Option<WindowDragHandler>,
}

impl WindowDragRegionState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        cx.observe_window_activation(window, |state, window, cx| {
            if window.is_window_active() {
                return;
            }
            let events = state.finish(WindowDragFinishReason::WindowDeactivated);
            emit_events(state.handler.clone(), events, window, cx);
        })
        .detach();
        cx.on_release_in(window, |state, window, cx| {
            let events = state.finish(WindowDragFinishReason::RegionRemoved);
            schedule_events(state.handler.clone(), events, window, cx);
        })
        .detach();
        Self {
            enabled: false,
            drag_threshold: px(DEFAULT_DRAG_THRESHOLD),
            status: WindowDragRegionStatus::new(),
            pointer: PointerLifecycle::Idle,
            next_interaction_id: 1,
            handler: None,
        }
    }

    fn synchronize(
        &mut self,
        enabled: bool,
        drag_threshold: Pixels,
        status: WindowDragRegionStatus,
        handler: Option<WindowDragHandler>,
    ) -> Vec<WindowDragRegionEvent> {
        self.handler = handler;
        self.drag_threshold = bounded_drag_threshold(drag_threshold);
        if let PointerLifecycle::Pressed(press) | PointerLifecycle::MoveRequested(press) =
            self.pointer
        {
            self.status.finish(press.interaction);
            status.begin(press.interaction);
        }
        self.status = status;
        if self.enabled == enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if enabled {
            Vec::new()
        } else {
            self.finish(WindowDragFinishReason::Disabled)
        }
    }

    fn owns_pointer_stream(&self) -> bool {
        !matches!(self.pointer, PointerLifecycle::Idle)
    }

    fn allocate_interaction(&mut self) -> WindowDragInteractionId {
        let interaction = WindowDragInteractionId(self.next_interaction_id);
        self.next_interaction_id = self.next_interaction_id.wrapping_add(1).max(1);
        interaction
    }

    fn pointer_down(&mut self, position: Point<Pixels>) -> Vec<WindowDragRegionEvent> {
        if !self.enabled || !point_is_finite(position) {
            return Vec::new();
        }
        if matches!(self.pointer, PointerLifecycle::CancelledUntilRelease) {
            self.pointer = PointerLifecycle::Idle;
        }
        if !matches!(self.pointer, PointerLifecycle::Idle) {
            return Vec::new();
        }
        let press = OwnedPress {
            interaction: self.allocate_interaction(),
            origin: position,
            current: position,
        };
        self.pointer = PointerLifecycle::Pressed(press);
        self.status.begin(press.interaction);
        vec![WindowDragRegionEvent::InteractionStarted {
            interaction: press.interaction,
        }]
    }

    fn double_activation(&mut self) -> Vec<WindowDragRegionEvent> {
        if !matches!(self.pointer, PointerLifecycle::Idle) {
            return Vec::new();
        }
        self.pointer = PointerLifecycle::CancelledUntilRelease;
        vec![WindowDragRegionEvent::DoubleActivationRequested]
    }

    fn suppress_click_release(&mut self) {
        if matches!(self.pointer, PointerLifecycle::Idle) {
            self.pointer = PointerLifecycle::CancelledUntilRelease;
        }
    }

    fn pointer_move(
        &mut self,
        position: Point<Pixels>,
        pressed_button: Option<MouseButton>,
    ) -> Vec<WindowDragRegionEvent> {
        if matches!(self.pointer, PointerLifecycle::CancelledUntilRelease) {
            if pressed_button != Some(MouseButton::Left) {
                self.pointer = PointerLifecycle::Idle;
            }
            return Vec::new();
        }
        if matches!(self.pointer, PointerLifecycle::Idle) {
            return Vec::new();
        }
        if pressed_button != Some(MouseButton::Left) {
            return self.finish(WindowDragFinishReason::PointerButtonLost);
        }
        if !point_is_finite(position) {
            return Vec::new();
        }

        match self.pointer {
            PointerLifecycle::Pressed(mut press) => {
                press.current = position;
                if crossed_threshold(press, self.drag_threshold) {
                    self.pointer = PointerLifecycle::MoveRequested(press);
                    vec![WindowDragRegionEvent::MoveRequested {
                        interaction: press.interaction,
                    }]
                } else {
                    self.pointer = PointerLifecycle::Pressed(press);
                    Vec::new()
                }
            }
            PointerLifecycle::MoveRequested(mut press) => {
                press.current = position;
                self.pointer = PointerLifecycle::MoveRequested(press);
                Vec::new()
            }
            PointerLifecycle::Idle | PointerLifecycle::CancelledUntilRelease => Vec::new(),
        }
    }

    fn pointer_up(&mut self) -> Vec<WindowDragRegionEvent> {
        if matches!(self.pointer, PointerLifecycle::CancelledUntilRelease) {
            self.pointer = PointerLifecycle::Idle;
            return Vec::new();
        }
        self.finish(WindowDragFinishReason::Completed)
    }

    fn finish(&mut self, reason: WindowDragFinishReason) -> Vec<WindowDragRegionEvent> {
        let press = match self.pointer {
            PointerLifecycle::Pressed(press) | PointerLifecycle::MoveRequested(press) => press,
            PointerLifecycle::Idle | PointerLifecycle::CancelledUntilRelease => return Vec::new(),
        };
        self.pointer = if reason.suppresses_pointer_until_release() {
            PointerLifecycle::CancelledUntilRelease
        } else {
            PointerLifecycle::Idle
        };
        self.status.finish(press.interaction);
        vec![WindowDragRegionEvent::InteractionFinished {
            interaction: press.interaction,
            reason,
        }]
    }
}

fn point_is_finite(point: Point<Pixels>) -> bool {
    f32::from(point.x).is_finite() && f32::from(point.y).is_finite()
}

fn crossed_threshold(press: OwnedPress, threshold: Pixels) -> bool {
    let delta_x = f32::from(press.current.x - press.origin.x);
    let delta_y = f32::from(press.current.y - press.origin.y);
    let threshold = f32::from(threshold);
    delta_x.mul_add(delta_x, delta_y * delta_y) >= threshold * threshold
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::prelude::*;
    use gpui::{
        Context, Entity, Modifiers, NavigationDirection, Render, TestAppContext, VisualTestContext,
        point,
    };

    use super::*;

    fn state() -> WindowDragRegionState {
        WindowDragRegionState {
            enabled: true,
            drag_threshold: px(4.0),
            status: WindowDragRegionStatus::new(),
            pointer: PointerLifecycle::Idle,
            next_interaction_id: 1,
            handler: None,
        }
    }

    #[test]
    fn status_should_follow_the_control_owned_interaction_lifecycle() {
        let mut state = state();
        let status = state.status.clone();
        state.pointer_down(point(px(10.0), px(10.0)));
        let active = status.active_interaction();
        state.pointer_move(point(px(20.0), px(10.0)), Some(MouseButton::Left));
        state.pointer_up();

        assert_eq!((active.is_some(), status.is_active()), (true, false));
    }

    #[test]
    fn movement_below_threshold_should_not_request_a_move() {
        let mut state = state();
        state.pointer_down(point(px(10.0), px(10.0)));
        let events = state.pointer_move(point(px(12.0), px(12.0)), Some(MouseButton::Left));

        assert!(events.is_empty());
    }

    #[test]
    fn threshold_crossing_should_request_one_move_for_continued_motion() {
        let mut state = state();
        state.pointer_down(point(px(10.0), px(10.0)));
        let mut events = state.pointer_move(point(px(14.0), px(10.0)), Some(MouseButton::Left));
        events.extend(state.pointer_move(point(px(30.0), px(10.0)), Some(MouseButton::Left)));

        assert!(matches!(
            events.as_slice(),
            [WindowDragRegionEvent::MoveRequested { .. }]
        ));
    }

    #[test]
    fn releases_before_and_after_threshold_should_clear_owned_press() {
        let mut state = state();
        state.pointer_down(point(px(10.0), px(10.0)));
        let before = state.pointer_up();
        state.pointer_down(point(px(20.0), px(20.0)));
        state.pointer_move(point(px(30.0), px(20.0)), Some(MouseButton::Left));
        let after = state.pointer_up();
        let reasons = before
            .into_iter()
            .chain(after)
            .filter_map(|event| match event {
                WindowDragRegionEvent::InteractionFinished { reason, .. } => Some(reason),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(reasons, [WindowDragFinishReason::Completed; 2]);
    }

    #[test]
    fn lost_primary_button_should_cancel_and_allow_a_clean_later_generation() {
        let mut state = state();
        let first = state.pointer_down(point(px(10.0), px(10.0)))[0];
        state.pointer_move(point(px(12.0), px(10.0)), None);
        let second = state.pointer_down(point(px(20.0), px(20.0)))[0];

        let interaction = |event| match event {
            WindowDragRegionEvent::InteractionStarted { interaction } => interaction,
            _ => panic!("expected an interaction-start event"),
        };
        assert_ne!(interaction(first), interaction(second));
    }

    #[test]
    fn cancellation_after_move_request_should_clear_press_and_report_the_exact_reason() {
        let mut state = state();
        state.pointer_down(point(px(10.0), px(10.0)));
        state.pointer_move(point(px(20.0), px(10.0)), Some(MouseButton::Left));
        let events = state.finish(WindowDragFinishReason::PointerExited);

        assert!(matches!(
            events.as_slice(),
            [WindowDragRegionEvent::InteractionFinished {
                reason: WindowDragFinishReason::PointerExited,
                ..
            }]
        ));
    }

    #[test]
    fn window_deactivation_after_threshold_should_finish_the_move_requested_state() {
        let mut state = state();
        state.pointer_down(point(px(10.0), px(10.0)));
        state.pointer_move(point(px(20.0), px(10.0)), Some(MouseButton::Left));
        let events = state.finish(WindowDragFinishReason::WindowDeactivated);

        assert!(matches!(
            events.as_slice(),
            [WindowDragRegionEvent::InteractionFinished {
                reason: WindowDragFinishReason::WindowDeactivated,
                ..
            }]
        ));
    }

    #[test]
    fn fresh_press_after_suppressed_cancellation_should_start_a_new_generation() {
        let mut state = state();
        let first = state.pointer_down(point(px(10.0), px(10.0)))[0];
        state.finish(WindowDragFinishReason::WindowDeactivated);
        let second = state.pointer_down(point(px(20.0), px(20.0)))[0];

        assert_ne!(first, second);
    }

    #[test]
    fn double_and_higher_clicks_should_suppress_release_without_starting_a_drag() {
        let mut state = state();
        let events = state.double_activation();
        let release = state.pointer_up();
        state.suppress_click_release();

        assert_eq!(
            (events, release, state.pointer),
            (
                vec![WindowDragRegionEvent::DoubleActivationRequested],
                Vec::new(),
                PointerLifecycle::CancelledUntilRelease,
            )
        );
    }

    #[test]
    fn drag_threshold_should_be_bounded_and_reject_non_finite_values() {
        assert_eq!(bounded_drag_threshold(px(0.0)), px(1.0));
        assert_eq!(bounded_drag_threshold(px(40.0)), px(32.0));
        assert_eq!(bounded_drag_threshold(px(f32::NAN)), px(4.0));
    }

    #[test]
    fn pointer_inset_should_be_nonnegative_bounded_and_finite() {
        assert_eq!(bounded_pointer_inset(px(-1.0)), px(0.0));
        assert_eq!(bounded_pointer_inset(px(12.0)), px(12.0));
        assert_eq!(
            bounded_pointer_inset(px(MAXIMUM_POINTER_INSET + 1.0)),
            px(MAXIMUM_POINTER_INSET)
        );
        assert_eq!(bounded_pointer_inset(px(f32::NAN)), px(0.0));
    }

    struct TestRoot {
        events: Rc<RefCell<Vec<WindowDragRegionEvent>>>,
        parent_events: Rc<RefCell<Vec<MouseButton>>>,
        parent_moves: Rc<RefCell<usize>>,
        child_presses: Rc<RefCell<usize>>,
        disabled: bool,
        show: bool,
        pointer_insets: Edges<Pixels>,
        overlay: bool,
        capture_overlay: bool,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let region_events = Rc::clone(&self.events);
            let parent_down_events = Rc::clone(&self.parent_events);
            let parent_up_events = Rc::clone(&self.parent_events);
            let parent_moves = Rc::clone(&self.parent_moves);
            let child_presses = Rc::clone(&self.child_presses);
            let content = div().relative().size_full().child(
                div()
                    .id("window-drag-child")
                    .debug_selector(|| "window-drag-child".into())
                    .absolute()
                    .right_0()
                    .top_0()
                    .w(px(48.0))
                    .h_full()
                    .block_mouse_except_scroll()
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        *child_presses.borrow_mut() += 1;
                    }),
            );
            let region = WindowDragRegion::new(
                "test-window-drag-region-control",
                "Move test Operating-System Window",
                content,
            )
            .disabled(self.disabled)
            .pointer_insets(self.pointer_insets)
            .debug_selector("test-window-drag-region")
            .on_event(move |event, _, _| region_events.borrow_mut().push(*event));

            div()
                .relative()
                .w(px(240.0))
                .h(px(80.0))
                .on_any_mouse_down(move |event, _, _| {
                    parent_down_events.borrow_mut().push(event.button);
                })
                .on_mouse_up(MouseButton::Left, move |event, _, _| {
                    parent_up_events.borrow_mut().push(event.button);
                })
                .on_mouse_move(move |_, _, _| *parent_moves.borrow_mut() += 1)
                .when(self.show, |root| root.child(region))
                .when(self.overlay, |root| {
                    root.child(
                        div()
                            .id("window-drag-overlay")
                            .debug_selector(|| "window-drag-overlay".into())
                            .absolute()
                            .left_0()
                            .top_0()
                            .w(px(80.0))
                            .h_full()
                            .occlude(),
                    )
                })
                .when(self.capture_overlay, |root| {
                    root.child(
                        div()
                            .id("window-drag-capture-overlay")
                            .debug_selector(|| "window-drag-capture-overlay".into())
                            .absolute()
                            .left_0()
                            .top_0()
                            .w(px(80.0))
                            .h_full()
                            .capture_any_mouse_down(|_, _, cx| cx.stop_propagation()),
                    )
                })
        }
    }

    struct MultipleRoot {
        first_events: Rc<RefCell<Vec<WindowDragRegionEvent>>>,
        second_events: Rc<RefCell<Vec<WindowDragRegionEvent>>>,
    }

    impl Render for MultipleRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let first_events = Rc::clone(&self.first_events);
            let second_events = Rc::clone(&self.second_events);
            div()
                .relative()
                .w(px(240.0))
                .h(px(80.0))
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .w(px(100.0))
                        .h_full()
                        .child(
                            WindowDragRegion::new(
                                "first-window-drag-region-control",
                                "First Operating-System Window drag region",
                                div().size_full(),
                            )
                            .debug_selector("first-window-drag-region")
                            .on_event(move |event, _, _| {
                                first_events.borrow_mut().push(*event);
                            }),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .right_0()
                        .top_0()
                        .w(px(100.0))
                        .h_full()
                        .child(
                            WindowDragRegion::new(
                                "second-window-drag-region-control",
                                "Second Operating-System Window drag region",
                                div().size_full(),
                            )
                            .debug_selector("second-window-drag-region")
                            .on_event(move |event, _, _| {
                                second_events.borrow_mut().push(*event);
                            }),
                        ),
                )
        }
    }

    struct DragWindow<'a> {
        root: Entity<TestRoot>,
        events: Rc<RefCell<Vec<WindowDragRegionEvent>>>,
        parent_events: Rc<RefCell<Vec<MouseButton>>>,
        parent_moves: Rc<RefCell<usize>>,
        child_presses: Rc<RefCell<usize>>,
        cx: &'a mut VisualTestContext,
    }

    fn drag_window(cx: &mut TestAppContext) -> DragWindow<'_> {
        let events = Rc::new(RefCell::new(Vec::new()));
        let parent_events = Rc::new(RefCell::new(Vec::new()));
        let parent_moves = Rc::new(RefCell::new(0));
        let child_presses = Rc::new(RefCell::new(0));
        let root_events = Rc::clone(&events);
        let root_parent_events = Rc::clone(&parent_events);
        let root_parent_moves = Rc::clone(&parent_moves);
        let root_child_presses = Rc::clone(&child_presses);
        let (root, cx) = cx.add_window_view(move |_, _| TestRoot {
            events: root_events,
            parent_events: root_parent_events,
            parent_moves: root_parent_moves,
            child_presses: root_child_presses,
            disabled: false,
            show: true,
            pointer_insets: Edges::default(),
            overlay: false,
            capture_overlay: false,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        DragWindow {
            root,
            events,
            parent_events,
            parent_moves,
            child_presses,
            cx,
        }
    }

    fn region_bounds(cx: &mut VisualTestContext) -> gpui::Bounds<Pixels> {
        cx.debug_bounds("test-window-drag-region")
            .expect("window drag region was not rendered")
    }

    #[gpui::test]
    fn pointer_insets_should_remove_reserved_edges_from_drag_ownership(cx: &mut TestAppContext) {
        let DragWindow {
            root, events, cx, ..
        } = drag_window(cx);
        root.update(cx, |root, cx| {
            root.pointer_insets = Edges {
                right: px(20.0),
                left: px(12.0),
                ..Edges::default()
            };
            cx.notify();
        });
        cx.run_until_parked();
        let root_bounds = region_bounds(cx);
        let target = cx
            .debug_bounds("test-window-drag-region-hitbox")
            .expect("the inset Window drag target was not rendered");

        assert_eq!(
            target,
            gpui::Bounds::new(
                point(root_bounds.left() + px(12.0), root_bounds.top()),
                gpui::size(root_bounds.size.width - px(32.0), root_bounds.size.height),
            )
        );

        let reserved = point(root_bounds.right() - px(4.0), root_bounds.center().y);
        cx.simulate_mouse_down(reserved, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(reserved.x + px(8.0), reserved.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(reserved, MouseButton::Left, Modifiers::none());

        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn captured_drag_should_continue_and_release_outside_without_ancestor_leakage(
        cx: &mut TestAppContext,
    ) {
        let DragWindow {
            events,
            parent_events,
            cx,
            ..
        } = drag_window(cx);
        let start = point(
            region_bounds(cx).right() - px(80.0),
            region_bounds(cx).center().y,
        );
        let outside = point(region_bounds(cx).right() + px(100.0), start.y + px(40.0));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(outside.x + px(20.0), outside.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());

        let events = events.borrow();
        assert!(matches!(
            events.as_slice(),
            [
                WindowDragRegionEvent::InteractionStarted { .. },
                WindowDragRegionEvent::MoveRequested { .. },
                WindowDragRegionEvent::InteractionFinished {
                    reason: WindowDragFinishReason::Completed,
                    ..
                }
            ]
        ));
        assert!(parent_events.borrow().is_empty());
    }

    #[gpui::test]
    fn macos_exit_without_a_reported_button_should_suppress_held_motion_and_release(
        cx: &mut TestAppContext,
    ) {
        let DragWindow {
            events,
            parent_events,
            parent_moves,
            cx,
            ..
        } = drag_window(cx);
        let start = point(
            region_bounds(cx).right() - px(80.0),
            region_bounds(cx).center().y,
        );
        let outside = point(region_bounds(cx).right() + px(20.0), start.y);

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_event(MouseExitEvent {
            position: outside,
            pressed_button: None,
            modifiers: Modifiers::none(),
        });
        cx.simulate_mouse_move(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());

        assert!(matches!(
            events.borrow().as_slice(),
            [
                WindowDragRegionEvent::InteractionStarted { .. },
                WindowDragRegionEvent::InteractionFinished {
                    reason: WindowDragFinishReason::PointerExited,
                    ..
                }
            ]
        ));
        assert_eq!(
            (*parent_moves.borrow(), parent_events.borrow().len()),
            (0, 0)
        );
    }

    #[gpui::test]
    fn secondary_middle_and_navigation_buttons_should_pass_through_unchanged(
        cx: &mut TestAppContext,
    ) {
        let DragWindow {
            events,
            parent_events,
            cx,
            ..
        } = drag_window(cx);
        let position = point(
            region_bounds(cx).right() - px(80.0),
            region_bounds(cx).center().y,
        );
        let buttons = [
            MouseButton::Right,
            MouseButton::Middle,
            MouseButton::Navigate(NavigationDirection::Back),
            MouseButton::Navigate(NavigationDirection::Forward),
        ];

        for button in buttons {
            cx.simulate_event(MouseDownEvent {
                button,
                position,
                modifiers: Modifiers::none(),
                click_count: 1,
                first_mouse: false,
            });
        }

        assert!(events.borrow().is_empty());
        assert_eq!(parent_events.borrow().as_slice(), buttons);
    }

    #[gpui::test]
    fn interactive_child_and_frontmost_overlays_should_block_the_region(cx: &mut TestAppContext) {
        let DragWindow {
            root,
            events,
            child_presses,
            cx,
            ..
        } = drag_window(cx);
        let child = cx
            .debug_bounds("window-drag-child")
            .expect("interactive child was not rendered")
            .center();
        cx.simulate_mouse_down(child, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(child, MouseButton::Left, Modifiers::none());

        root.update(cx, |root, cx| {
            root.overlay = true;
            cx.notify();
        });
        cx.run_until_parked();
        let overlay = cx
            .debug_bounds("window-drag-overlay")
            .expect("occluding overlay was not rendered")
            .center();
        cx.simulate_mouse_down(overlay, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(overlay, MouseButton::Left, Modifiers::none());

        root.update(cx, |root, cx| {
            root.overlay = false;
            root.capture_overlay = true;
            cx.notify();
        });
        cx.run_until_parked();
        let capture_overlay = cx
            .debug_bounds("window-drag-capture-overlay")
            .expect("capture overlay was not rendered")
            .center();
        cx.simulate_mouse_down(capture_overlay, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(capture_overlay, MouseButton::Left, Modifiers::none());

        assert_eq!(*child_presses.borrow(), 1);
        assert!(events.borrow().is_empty());
    }

    #[gpui::test]
    fn disablement_deactivation_and_removal_should_finish_owned_interactions(
        cx: &mut TestAppContext,
    ) {
        let DragWindow {
            root, events, cx, ..
        } = drag_window(cx);
        let start = point(
            region_bounds(cx).right() - px(80.0),
            region_bounds(cx).center().y,
        );
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());

        root.update(cx, |root, cx| {
            root.disabled = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.deactivate_window();
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());
        cx.update(|window, _| window.activate_window());

        root.update(cx, |root, cx| {
            root.show = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        root.update(cx, |root, cx| {
            root.show = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        let reasons = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                WindowDragRegionEvent::InteractionFinished { reason, .. } => Some(*reason),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            reasons,
            [
                WindowDragFinishReason::Disabled,
                WindowDragFinishReason::WindowDeactivated,
                WindowDragFinishReason::RegionRemoved,
            ]
        );
    }

    #[gpui::test]
    fn multiple_regions_should_keep_pointer_interactions_independent(cx: &mut TestAppContext) {
        let first_events = Rc::new(RefCell::new(Vec::new()));
        let second_events = Rc::new(RefCell::new(Vec::new()));
        let root_first_events = Rc::clone(&first_events);
        let root_second_events = Rc::clone(&second_events);
        let (_, cx) = cx.add_window_view(move |_, _| MultipleRoot {
            first_events: root_first_events,
            second_events: root_second_events,
        });
        cx.run_until_parked();
        let second = cx
            .debug_bounds("second-window-drag-region")
            .expect("second region was not rendered")
            .center();

        cx.simulate_mouse_down(second, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(second.x + px(12.0), second.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::none());

        assert!(first_events.borrow().is_empty());
        assert!(matches!(
            second_events.borrow().as_slice(),
            [
                WindowDragRegionEvent::InteractionStarted { .. },
                WindowDragRegionEvent::MoveRequested { .. },
                WindowDragRegionEvent::InteractionFinished { .. }
            ]
        ));
    }

    #[gpui::test]
    fn double_and_triple_clicks_should_never_request_a_move(cx: &mut TestAppContext) {
        let DragWindow { events, cx, .. } = drag_window(cx);
        let position = point(
            region_bounds(cx).right() - px(80.0),
            region_bounds(cx).center().y,
        );
        for click_count in [2, 3] {
            cx.simulate_event(MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::none(),
                click_count,
                first_mouse: false,
            });
            cx.simulate_event(MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Modifiers::none(),
                click_count,
            });
        }

        assert_eq!(
            events.borrow().as_slice(),
            [WindowDragRegionEvent::DoubleActivationRequested]
        );
    }
}
