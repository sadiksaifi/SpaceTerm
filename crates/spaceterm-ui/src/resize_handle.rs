use std::ops::RangeInclusive;
use std::rc::Rc;

use gpui::{
    App, CursorStyle, ElementId, FocusHandle, Global, HitboxBehavior, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    ParentElement, Pixels, Point, RenderOnce, Rgba, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, prelude::FluentBuilder as _, px,
};

const DEFAULT_KEYBOARD_STEP: f32 = 1.0;
const DEFAULT_MODIFIED_KEYBOARD_STEP: f32 = 10.0;
const MINIMUM_METRIC: f32 = 0.5;
const MAXIMUM_METRIC: f32 = 32.0;

/// The logical movement axis owned by a [`ResizeHandle`].
///
/// Horizontal means pointer movement along the x-axis and uses a column-resize cursor. Vertical
/// means pointer movement along the y-axis and uses a row-resize cursor. The names describe
/// movement, not the visual orientation of the divider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeAxis {
    /// Pointer movement along the x-axis with a column-resize cursor.
    Horizontal,
    /// Pointer movement along the y-axis with a row-resize cursor.
    Vertical,
}

impl ResizeAxis {
    fn coordinate(self, point: Point<Pixels>) -> f32 {
        match self {
            Self::Horizontal => f32::from(point.x),
            Self::Vertical => f32::from(point.y),
        }
    }

    fn cursor(self) -> CursorStyle {
        match self {
            Self::Horizontal => CursorStyle::ResizeColumn,
            Self::Vertical => CursorStyle::ResizeRow,
        }
    }

    fn keyboard_direction(self, key: &str) -> Option<f32> {
        match (self, key) {
            (Self::Horizontal, "left") | (Self::Vertical, "up") => Some(-1.0),
            (Self::Horizontal, "right") | (Self::Vertical, "down") => Some(1.0),
            _ => None,
        }
    }
}

/// Stable identity for one resize interaction.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResizeInteractionId(u64);

impl ResizeInteractionId {
    /// Returns the monotonic numeric identity.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The input path that initiated a resize request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeInputSource {
    /// A primary-pointer interaction.
    Pointer,
    /// An axis-appropriate arrow key.
    Keyboard,
}

/// Why an owned resize interaction finished.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResizeFinishReason {
    /// The pointer button was released or a keyboard request completed normally.
    Completed,
    /// Escape cancelled an interaction and requested restoration where possible.
    Escape,
    /// A move arrived after the primary pointer button was lost.
    PointerButtonLost,
    /// The Operating-System Window became inactive.
    WindowDeactivated,
    /// The handle became disabled while it owned an interaction.
    Disabled,
    /// The keyed control state was released while it owned an interaction.
    HandleRemoved,
}

/// A typed request emitted by [`ResizeHandle`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResizeHandleEvent {
    /// The control took ownership of a new resize interaction.
    InteractionStarted {
        /// Stable identity for this interaction.
        interaction: ResizeInteractionId,
        /// Input path that initiated the interaction.
        source: ResizeInputSource,
        /// Authoritative caller value observed at the start.
        original_value: f32,
    },
    /// The caller should apply its product policy to a requested value.
    ResizeRequested {
        /// Stable identity for this interaction.
        interaction: ResizeInteractionId,
        /// Input path that produced the request.
        source: ResizeInputSource,
        /// Cumulative displacement from the original value.
        displacement: f32,
        /// Requested logical value after the control's optional logical range.
        requested_value: f32,
    },
    /// The optional reset gesture was activated.
    ResetRequested {
        /// Input path that activated reset.
        source: ResizeInputSource,
    },
    /// The control released ownership of an interaction.
    InteractionFinished {
        /// Stable identity for this interaction.
        interaction: ResizeInteractionId,
        /// Input path that initiated the interaction.
        source: ResizeInputSource,
        /// Why ownership ended.
        reason: ResizeFinishReason,
    },
}

/// Application-owned divider colors for every resize-handle state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeHandlePaint {
    normal: Rgba,
    hovered: Rgba,
    active: Rgba,
    focused: Rgba,
    disabled: Rgba,
}

impl ResizeHandlePaint {
    /// Creates the complete bounded resize-handle paint catalog.
    pub fn new(normal: Rgba, hovered: Rgba, active: Rgba, focused: Rgba, disabled: Rgba) -> Self {
        Self {
            normal,
            hovered,
            active,
            focused,
            disabled,
        }
    }
}

/// Compact metrics used by every resize handle in an application.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeHandleMetrics {
    visible_thickness: Pixels,
    hitbox_thickness: Pixels,
    hover_thickness: Pixels,
    active_thickness: Pixels,
    focus_thickness: Pixels,
}

impl ResizeHandleMetrics {
    /// Creates bounded metrics with the supplied visible and pointer-hitbox thicknesses.
    pub fn new(visible_thickness: Pixels, hitbox_thickness: Pixels) -> Self {
        Self {
            visible_thickness,
            hitbox_thickness,
            hover_thickness: visible_thickness,
            active_thickness: visible_thickness,
            focus_thickness: visible_thickness,
        }
        .normalized()
    }

    /// Sets the visible emphasis used while hovered.
    pub fn hover_thickness(mut self, thickness: Pixels) -> Self {
        self.hover_thickness = thickness;
        self.normalized()
    }

    /// Sets the visible emphasis used during an active pointer interaction.
    pub fn active_thickness(mut self, thickness: Pixels) -> Self {
        self.active_thickness = thickness;
        self.normalized()
    }

    /// Sets the visible emphasis used while the handle has keyboard focus.
    pub fn focus_thickness(mut self, thickness: Pixels) -> Self {
        self.focus_thickness = thickness;
        self.normalized()
    }

    /// Returns the layout thickness reserved by the divider.
    pub fn visible_thickness(self) -> Pixels {
        self.visible_thickness
    }

    /// Returns the larger pointer target thickness.
    pub fn hitbox_thickness(self) -> Pixels {
        self.hitbox_thickness
    }

    fn normalized(mut self) -> Self {
        self.visible_thickness = bounded_metric(self.visible_thickness);
        self.hover_thickness = bounded_metric(self.hover_thickness);
        self.active_thickness = bounded_metric(self.active_thickness);
        self.focus_thickness = bounded_metric(self.focus_thickness);
        let emphasis = f32::from(self.visible_thickness)
            .max(f32::from(self.hover_thickness))
            .max(f32::from(self.active_thickness))
            .max(f32::from(self.focus_thickness));
        self.hitbox_thickness = px(f32::from(bounded_metric(self.hitbox_thickness)).max(emphasis));
        self
    }
}

fn bounded_metric(metric: Pixels) -> Pixels {
    let value = f32::from(metric);
    px(if value.is_finite() {
        value.clamp(MINIMUM_METRIC, MAXIMUM_METRIC)
    } else {
        MINIMUM_METRIC
    })
}

/// Application-installed presentation for every [`ResizeHandle`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeHandleTheme {
    paint: ResizeHandlePaint,
    metrics: ResizeHandleMetrics,
}

impl ResizeHandleTheme {
    /// Creates a complete theme from application-owned paint and bounded metrics.
    pub fn new(paint: ResizeHandlePaint, metrics: ResizeHandleMetrics) -> Self {
        Self { paint, metrics }
    }

    /// Returns the divider thickness that callers may use in layout calculations.
    pub fn visible_thickness(self) -> Pixels {
        self.metrics.visible_thickness
    }

    /// Returns the pointer target thickness.
    pub fn hitbox_thickness(self) -> Pixels {
        self.metrics.hitbox_thickness
    }
}

impl Global for ResizeHandleTheme {}

type ResizeHandler = Rc<dyn Fn(&ResizeHandleEvent, &mut Window, &mut App)>;

/// A platform-neutral GPUI divider that owns resize input and presentation mechanics.
///
/// The control owns its enlarged hitbox, pointer capture, cumulative displacement, keyboard
/// interaction, cancellation, focus, and themed visual states. It never owns Pane Layout ratios,
/// panel dimensions, minimum sizes, collapse behavior, or other application policy. Each
/// [`ResizeHandleEvent::ResizeRequested`] is advisory: callers apply policy and feed their
/// authoritative value back on the next render. Caller clamping never rebases an active drag.
///
/// A logical accessibility name is mandatory. GPUI 0.2.2 cannot yet publish a custom separator
/// role and value to the native accessibility tree, but retaining these semantics in the public
/// interface keeps handles named and makes the framework seam explicit.
#[derive(IntoElement)]
pub struct ResizeHandle {
    id: ElementId,
    accessibility_name: SharedString,
    axis: ResizeAxis,
    current_value: f32,
    range: Option<RangeInclusive<f32>>,
    disabled: bool,
    tab_stop: bool,
    reset_on_double_click: bool,
    keyboard_step: f32,
    modified_keyboard_step: f32,
    debug_selector: Option<String>,
    on_event: Option<ResizeHandler>,
}

impl ResizeHandle {
    /// Creates a resize handle with stable identity and a mandatory logical accessibility name.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        axis: ResizeAxis,
        current_value: f32,
    ) -> Self {
        Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            axis,
            current_value: finite_or_zero(current_value),
            range: None,
            disabled: false,
            tab_stop: false,
            reset_on_double_click: false,
            keyboard_step: DEFAULT_KEYBOARD_STEP,
            modified_keyboard_step: DEFAULT_MODIFIED_KEYBOARD_STEP,
            debug_selector: None,
            on_event: None,
        }
    }

    /// Constrains requested logical values without transferring application policy to the control.
    pub fn range(mut self, range: RangeInclusive<f32>) -> Self {
        let start = *range.start();
        let end = *range.end();
        self.range = (start.is_finite() && end.is_finite() && start <= end).then_some(start..=end);
        self
    }

    /// Controls whether the handle accepts pointer and keyboard resize input.
    ///
    /// Disabling a keyboard-focused handle also advances focus so it cannot strand the responder.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on this handle.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    /// Enables the optional primary-pointer double-click reset request.
    pub fn reset_on_double_click(mut self, enabled: bool) -> Self {
        self.reset_on_double_click = enabled;
        self
    }

    /// Configures the ordinary and Shift-modified keyboard steps.
    pub fn keyboard_steps(mut self, ordinary: f32, modified: f32) -> Self {
        if ordinary.is_finite() && ordinary > 0.0 {
            self.keyboard_step = ordinary;
        }
        if modified.is_finite() && modified > 0.0 {
            self.modified_keyboard_step = modified;
        }
        self
    }

    /// Adds a stable root selector. The hitbox and divider append `-hitbox` and `-divider`.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Handles ordered resize lifecycle requests.
    pub fn on_event(
        mut self,
        handler: impl Fn(&ResizeHandleEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_event = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ResizeHandle {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.global::<ResizeHandleTheme>();
        let enabled = !self.disabled && self.on_event.is_some();
        let state = window.use_keyed_state(self.id.clone(), cx, ResizeHandleState::new);
        let cancelled = state.update(cx, |state, cx| {
            state.synchronize(
                enabled,
                self.tab_stop,
                self.axis,
                self.current_value,
                self.range.clone(),
                self.keyboard_step,
                self.modified_keyboard_step,
                self.on_event.clone(),
                cx,
            )
        });
        emit_events(self.on_event.clone(), cancelled, window, cx);

        let (focus_handle, hovered, active) = {
            let state = state.read(cx);
            (
                state.focus_handle.clone(),
                state.hovered,
                state.pointer.is_some(),
            )
        };
        if !enabled && focus_handle.is_focused(window) {
            window.focus_next();
            if focus_handle.is_focused(window) {
                window.blur();
            }
        }
        let focused = focus_handle.is_focused(window);
        let (color, divider_thickness) =
            resolve_presentation(theme, enabled, hovered, active, focused);
        let root_selector = self
            .debug_selector
            .unwrap_or_else(|| self.accessibility_name.to_string());
        let hitbox_selector = format!("{root_selector}-hitbox");
        let divider_selector = format!("{root_selector}-divider");

        let hover_state = state.clone();
        let down_state = state.clone();
        let move_state = state.clone();
        let up_state = state.clone();
        let pointer_handler = self.on_event.clone();
        let reset_handler = self.on_event.clone();
        let pointer_focus = focus_handle.clone();
        let reset_on_double_click = self.reset_on_double_click;
        let pointer_tracker = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let down_hitbox = hitbox.clone();
                let down_handler = pointer_handler.clone();
                let move_handler = pointer_handler.clone();
                let up_handler = pointer_handler.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || !down_hitbox.is_hovered(window)
                    {
                        return;
                    }
                    if event.click_count == 2 && reset_on_double_click {
                        if !down_state.read(cx).enabled {
                            return;
                        }
                        window.prevent_default();
                        pointer_focus.focus(window);
                        emit_events(
                            reset_handler.clone(),
                            vec![ResizeHandleEvent::ResetRequested {
                                source: ResizeInputSource::Pointer,
                            }],
                            window,
                            cx,
                        );
                        cx.stop_propagation();
                        return;
                    }
                    if event.click_count != 1 {
                        return;
                    }
                    let events =
                        down_state.update(cx, |state, cx| state.pointer_down(event.position, cx));
                    if !events.is_empty() {
                        pointer_focus.focus(window);
                        window.prevent_default();
                        emit_events(down_handler.clone(), events, window, cx);
                        cx.stop_propagation();
                    }
                });

                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.capture() || move_state.read(cx).pointer.is_none() {
                        return;
                    }
                    let events = move_state.update(cx, |state, cx| {
                        state.pointer_move(event.position, event.pressed_button, cx)
                    });
                    window.prevent_default();
                    emit_events(move_handler.clone(), events, window, cx);
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || up_state.read(cx).pointer.is_none()
                    {
                        return;
                    }
                    let events = up_state.update(cx, |state, cx| {
                        state.finish_pointer(ResizeFinishReason::Completed, false, cx)
                    });
                    window.prevent_default();
                    emit_events(up_handler.clone(), events, window, cx);
                    cx.stop_propagation();
                });
            },
        )
        .absolute()
        .inset_0();

        let hitbox_offset = px(-(f32::from(theme.metrics.hitbox_thickness)
            - f32::from(theme.metrics.visible_thickness))
            / 2.0);
        let hitbox_debug = hitbox_selector.clone();
        let divider_debug = divider_selector.clone();
        let hitbox = div()
            .id("resize-handle-hitbox")
            .debug_selector(move || hitbox_debug)
            .absolute()
            .flex()
            .items_center()
            .justify_center()
            .block_mouse_except_scroll()
            .when(enabled, |hitbox| hitbox.cursor(self.axis.cursor()))
            .when(!enabled, |hitbox| hitbox.cursor_default())
            .on_hover(move |hovered, _, cx| {
                hover_state.update(cx, |state, cx| state.set_hovered(*hovered, cx));
            })
            .inset_0()
            .child(
                div()
                    .id("resize-handle-divider")
                    .debug_selector(move || divider_debug)
                    .flex_shrink_0()
                    .bg(color)
                    .when(self.axis == ResizeAxis::Horizontal, |divider| {
                        divider.w(divider_thickness).h_full()
                    })
                    .when(self.axis == ResizeAxis::Vertical, |divider| {
                        divider.h(divider_thickness).w_full()
                    }),
            )
            .child(pointer_tracker);

        let key_state = state;
        let key_handler = self.on_event;
        let key_focus = focus_handle.clone();
        div()
            .id(self.id)
            .debug_selector(move || root_selector)
            .relative()
            .flex_shrink_0()
            .track_focus(&focus_handle)
            .when(self.axis == ResizeAxis::Horizontal, |root| {
                root.w(theme.metrics.hitbox_thickness)
                    .h_full()
                    .ml(hitbox_offset)
                    .mr(hitbox_offset)
            })
            .when(self.axis == ResizeAxis::Vertical, |root| {
                root.h(theme.metrics.hitbox_thickness)
                    .w_full()
                    .mt(hitbox_offset)
                    .mb(hitbox_offset)
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if !key_focus.is_focused(window) || !key_state.read(cx).enabled {
                    return;
                }
                let events = key_state.update(cx, |state, cx| state.key_down(event, cx));
                if events.is_empty() {
                    return;
                }
                window.prevent_default();
                emit_events(key_handler.clone(), events, window, cx);
                cx.stop_propagation();
            })
            .child(hitbox)
    }
}

fn resolve_presentation(
    theme: ResizeHandleTheme,
    enabled: bool,
    hovered: bool,
    active: bool,
    focused: bool,
) -> (Rgba, Pixels) {
    if !enabled {
        (theme.paint.disabled, theme.metrics.visible_thickness)
    } else if active {
        (theme.paint.active, theme.metrics.active_thickness)
    } else if focused {
        (theme.paint.focused, theme.metrics.focus_thickness)
    } else if hovered {
        (theme.paint.hovered, theme.metrics.hover_thickness)
    } else {
        (theme.paint.normal, theme.metrics.visible_thickness)
    }
}

fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

fn clamp_to_range(value: f32, range: Option<&RangeInclusive<f32>>) -> f32 {
    range.map_or(value, |range| value.clamp(*range.start(), *range.end()))
}

fn emit_events(
    handler: Option<ResizeHandler>,
    events: Vec<ResizeHandleEvent>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(handler) = handler else {
        return;
    };
    for event in events {
        handler(&event, window, cx);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PointerInteraction {
    id: ResizeInteractionId,
    original_coordinate: f32,
    original_value: f32,
    last_requested_value: Option<f32>,
}

struct ResizeHandleState {
    focus_handle: FocusHandle,
    axis: ResizeAxis,
    current_value: f32,
    keyboard_value: f32,
    range: Option<RangeInclusive<f32>>,
    keyboard_step: f32,
    modified_keyboard_step: f32,
    enabled: bool,
    hovered: bool,
    pointer: Option<PointerInteraction>,
    next_interaction_id: u64,
    handler: Option<ResizeHandler>,
}

impl ResizeHandleState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();
        cx.observe_window_activation(window, |state, window, cx| {
            if window.is_window_active() {
                return;
            }
            let events = state.finish_pointer(ResizeFinishReason::WindowDeactivated, false, cx);
            emit_events(state.handler.clone(), events, window, cx);
        })
        .detach();
        cx.on_release_in(window, |state, window, cx| {
            let events =
                state.finish_pointer_without_context(ResizeFinishReason::HandleRemoved, false);
            emit_events(state.handler.clone(), events, window, cx);
        })
        .detach();
        Self {
            focus_handle,
            axis: ResizeAxis::Horizontal,
            current_value: 0.0,
            keyboard_value: 0.0,
            range: None,
            keyboard_step: DEFAULT_KEYBOARD_STEP,
            modified_keyboard_step: DEFAULT_MODIFIED_KEYBOARD_STEP,
            enabled: false,
            hovered: false,
            pointer: None,
            next_interaction_id: 1,
            handler: None,
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "render synchronization mirrors the bounded public control configuration"
    )]
    fn synchronize(
        &mut self,
        enabled: bool,
        tab_stop: bool,
        axis: ResizeAxis,
        current_value: f32,
        range: Option<RangeInclusive<f32>>,
        keyboard_step: f32,
        modified_keyboard_step: f32,
        handler: Option<ResizeHandler>,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<ResizeHandleEvent> {
        self.handler = handler;
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && tab_stop);
        self.axis = axis;
        self.current_value = finite_or_zero(current_value);
        self.keyboard_value = self.current_value;
        self.range = range;
        self.keyboard_step = keyboard_step;
        self.modified_keyboard_step = modified_keyboard_step;
        if self.enabled == enabled {
            return Vec::new();
        }
        self.enabled = enabled;
        if !enabled {
            self.hovered = false;
            return self.finish_pointer(ResizeFinishReason::Disabled, false, cx);
        }
        cx.notify();
        Vec::new()
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut gpui::Context<Self>) {
        let hovered = self.enabled && hovered;
        if self.hovered != hovered {
            self.hovered = hovered;
            cx.notify();
        }
    }

    fn allocate_interaction(&mut self) -> ResizeInteractionId {
        let id = ResizeInteractionId(self.next_interaction_id);
        self.next_interaction_id = self.next_interaction_id.wrapping_add(1).max(1);
        id
    }

    fn pointer_down(
        &mut self,
        position: Point<Pixels>,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<ResizeHandleEvent> {
        if !self.enabled || self.pointer.is_some() {
            return Vec::new();
        }
        let coordinate = self.axis.coordinate(position);
        if !coordinate.is_finite() {
            return Vec::new();
        }
        let interaction = PointerInteraction {
            id: self.allocate_interaction(),
            original_coordinate: coordinate,
            original_value: self.current_value,
            last_requested_value: None,
        };
        self.pointer = Some(interaction);
        cx.notify();
        vec![ResizeHandleEvent::InteractionStarted {
            interaction: interaction.id,
            source: ResizeInputSource::Pointer,
            original_value: interaction.original_value,
        }]
    }

    fn pointer_move(
        &mut self,
        position: Point<Pixels>,
        pressed_button: Option<MouseButton>,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<ResizeHandleEvent> {
        if self.pointer.is_none() {
            return Vec::new();
        }
        if pressed_button != Some(MouseButton::Left) {
            return self.finish_pointer(ResizeFinishReason::PointerButtonLost, false, cx);
        }
        let coordinate = self.axis.coordinate(position);
        if !coordinate.is_finite() {
            return Vec::new();
        }
        let Some(pointer) = self.pointer.as_mut() else {
            return Vec::new();
        };
        let displacement = coordinate - pointer.original_coordinate;
        let requested_value =
            clamp_to_range(pointer.original_value + displacement, self.range.as_ref());
        if pointer.last_requested_value == Some(requested_value) {
            return Vec::new();
        }
        pointer.last_requested_value = Some(requested_value);
        vec![ResizeHandleEvent::ResizeRequested {
            interaction: pointer.id,
            source: ResizeInputSource::Pointer,
            displacement,
            requested_value,
        }]
    }

    fn finish_pointer(
        &mut self,
        reason: ResizeFinishReason,
        restore: bool,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<ResizeHandleEvent> {
        let events = self.finish_pointer_without_context(reason, restore);
        if !events.is_empty() {
            cx.notify();
        }
        events
    }

    fn finish_pointer_without_context(
        &mut self,
        reason: ResizeFinishReason,
        restore: bool,
    ) -> Vec<ResizeHandleEvent> {
        let Some(pointer) = self.pointer.take() else {
            return Vec::new();
        };
        let mut events = Vec::with_capacity(if restore { 2 } else { 1 });
        if restore && pointer.last_requested_value != Some(pointer.original_value) {
            events.push(ResizeHandleEvent::ResizeRequested {
                interaction: pointer.id,
                source: ResizeInputSource::Pointer,
                displacement: 0.0,
                requested_value: pointer.original_value,
            });
        }
        events.push(ResizeHandleEvent::InteractionFinished {
            interaction: pointer.id,
            source: ResizeInputSource::Pointer,
            reason,
        });
        events
    }

    fn key_down(
        &mut self,
        event: &KeyDownEvent,
        cx: &mut gpui::Context<Self>,
    ) -> Vec<ResizeHandleEvent> {
        if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
            return self.finish_pointer(ResizeFinishReason::Escape, true, cx);
        }
        if self.pointer.is_some() {
            return Vec::new();
        }
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return Vec::new();
        }
        let Some(direction) = self.axis.keyboard_direction(&event.keystroke.key) else {
            return Vec::new();
        };
        let step = if modifiers.shift {
            self.modified_keyboard_step
        } else {
            self.keyboard_step
        };
        let displacement = direction * step;
        let original_value = self.keyboard_value;
        let requested_value = clamp_to_range(original_value + displacement, self.range.as_ref());
        self.keyboard_value = requested_value;
        let interaction = self.allocate_interaction();
        vec![
            ResizeHandleEvent::InteractionStarted {
                interaction,
                source: ResizeInputSource::Keyboard,
                original_value,
            },
            ResizeHandleEvent::ResizeRequested {
                interaction,
                source: ResizeInputSource::Keyboard,
                displacement,
                requested_value,
            },
            ResizeHandleEvent::InteractionFinished {
                interaction,
                source: ResizeInputSource::Keyboard,
                reason: ResizeFinishReason::Completed,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{
        Context, Entity, Modifiers, Render, TestAppContext, VisualTestContext, point, rgba,
    };

    use super::*;

    fn test_theme() -> ResizeHandleTheme {
        ResizeHandleTheme::new(
            ResizeHandlePaint::new(
                rgba(0x11_11_11_ff),
                rgba(0x22_22_22_ff),
                rgba(0x33_33_33_ff),
                rgba(0x44_44_44_ff),
                rgba(0x55_55_55_ff),
            ),
            ResizeHandleMetrics::new(px(1.0), px(9.0))
                .hover_thickness(px(2.0))
                .active_thickness(px(3.0))
                .focus_thickness(px(2.0)),
        )
    }

    #[test]
    fn axis_should_resolve_precise_cursor_and_keyboard_direction() {
        assert_eq!(ResizeAxis::Horizontal.cursor(), CursorStyle::ResizeColumn);
        assert_eq!(ResizeAxis::Vertical.cursor(), CursorStyle::ResizeRow);
        assert_eq!(
            ResizeAxis::Horizontal.keyboard_direction("right"),
            Some(1.0)
        );
        assert_eq!(ResizeAxis::Vertical.keyboard_direction("up"), Some(-1.0));
        assert_eq!(ResizeAxis::Horizontal.keyboard_direction("down"), None);
    }

    #[test]
    fn metrics_should_bound_values_and_keep_hitbox_larger_than_emphasis() {
        let metrics = ResizeHandleMetrics::new(px(f32::NAN), px(1.0)).active_thickness(px(40.0));

        assert_eq!(metrics.visible_thickness, px(MINIMUM_METRIC));
        assert_eq!(metrics.active_thickness, px(MAXIMUM_METRIC));
        assert_eq!(metrics.hitbox_thickness, px(MAXIMUM_METRIC));
    }

    #[test]
    fn presentation_should_prioritize_disabled_active_focused_hovered_and_normal_states() {
        let theme = test_theme();

        assert_eq!(
            resolve_presentation(theme, false, true, true, true),
            (theme.paint.disabled, theme.metrics.visible_thickness)
        );
        assert_eq!(
            resolve_presentation(theme, true, true, true, true),
            (theme.paint.active, theme.metrics.active_thickness)
        );
        assert_eq!(
            resolve_presentation(theme, true, true, false, true),
            (theme.paint.focused, theme.metrics.focus_thickness)
        );
        assert_eq!(
            resolve_presentation(theme, true, true, false, false),
            (theme.paint.hovered, theme.metrics.hover_thickness)
        );
        assert_eq!(
            resolve_presentation(theme, true, false, false, false),
            (theme.paint.normal, theme.metrics.visible_thickness)
        );
    }

    struct TestRoot {
        events: Rc<RefCell<Vec<ResizeHandleEvent>>>,
        axis: ResizeAxis,
        value: f32,
        disabled: bool,
        show: bool,
        tab_stop: bool,
        clamp: Option<(f32, f32)>,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let events = Rc::clone(&self.events);
            let root_entity = cx.entity().downgrade();
            let clamp = self.clamp;
            div().relative().size_full().when(self.show, |root| {
                root.child(
                    ResizeHandle::new("test-resize", "Resize test", self.axis, self.value)
                        .range(0.0..=200.0)
                        .disabled(self.disabled)
                        .tab_stop(self.tab_stop)
                        .reset_on_double_click(true)
                        .debug_selector("test-resize")
                        .on_event(move |event, _, cx| {
                            events.borrow_mut().push(*event);
                            if let (
                                Some((minimum, maximum)),
                                ResizeHandleEvent::ResizeRequested {
                                    requested_value, ..
                                },
                            ) = (clamp, event)
                            {
                                let requested_value = *requested_value;
                                let _ = root_entity.update(cx, |root, cx| {
                                    root.value = requested_value.clamp(minimum, maximum);
                                    cx.notify();
                                });
                            }
                        }),
                )
            })
        }
    }

    fn resize_window(
        cx: &mut TestAppContext,
        axis: ResizeAxis,
    ) -> (
        Entity<TestRoot>,
        Rc<RefCell<Vec<ResizeHandleEvent>>>,
        &mut VisualTestContext,
    ) {
        cx.set_global(test_theme());
        let events = Rc::new(RefCell::new(Vec::new()));
        let root_events = Rc::clone(&events);
        let (root, cx) = cx.add_window_view(move |_, _| TestRoot {
            events: root_events,
            axis,
            value: 100.0,
            disabled: false,
            show: true,
            tab_stop: true,
            clamp: None,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, events, cx)
    }

    struct MultipleRoot {
        first_events: Rc<RefCell<Vec<ResizeHandleEvent>>>,
        second_events: Rc<RefCell<Vec<ResizeHandleEvent>>>,
    }

    impl Render for MultipleRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let first_events = Rc::clone(&self.first_events);
            let second_events = Rc::clone(&self.second_events);
            div()
                .relative()
                .size_full()
                .child(
                    div().absolute().left(px(40.0)).top_0().h(px(100.0)).child(
                        ResizeHandle::new(
                            "first-resize",
                            "First resize",
                            ResizeAxis::Horizontal,
                            40.0,
                        )
                        .debug_selector("first-resize")
                        .on_event(move |event, _, _| first_events.borrow_mut().push(*event)),
                    ),
                )
                .child(
                    div().absolute().left(px(120.0)).top_0().h(px(100.0)).child(
                        ResizeHandle::new(
                            "second-resize",
                            "Second resize",
                            ResizeAxis::Horizontal,
                            120.0,
                        )
                        .debug_selector("second-resize")
                        .on_event(move |event, _, _| second_events.borrow_mut().push(*event)),
                    ),
                )
        }
    }

    fn hitbox(cx: &mut VisualTestContext) -> gpui::Bounds<Pixels> {
        cx.debug_bounds("test-resize-hitbox")
            .unwrap_or_else(|| panic!("resize hitbox was not rendered"))
    }

    #[gpui::test]
    fn horizontal_drag_should_emit_cumulative_displacement_and_ordered_lifecycle(
        cx: &mut TestAppContext,
    ) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();
        let first = point(start.x + px(12.0), start.y);
        let second = point(start.x + px(25.0), start.y);

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(first, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(second, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::none());

        let events = events.borrow();
        assert!(matches!(
            events[0],
            ResizeHandleEvent::InteractionStarted { .. }
        ));
        assert!(matches!(
            events[1],
            ResizeHandleEvent::ResizeRequested {
                displacement: 12.0,
                requested_value: 112.0,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            ResizeHandleEvent::ResizeRequested {
                displacement: 25.0,
                requested_value: 125.0,
                ..
            }
        ));
        assert!(matches!(
            events[3],
            ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::Completed,
                ..
            }
        ));
    }

    #[gpui::test]
    fn vertical_drag_should_continue_and_release_outside_original_bounds(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Vertical);
        let start = hitbox(cx).center();
        let outside = point(start.x + px(80.0), start.y + px(40.0));

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::none());

        let events = events.borrow();
        assert!(matches!(
            events[1],
            ResizeHandleEvent::ResizeRequested {
                displacement: 40.0,
                requested_value: 140.0,
                ..
            }
        ));
        assert!(matches!(
            events[2],
            ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::Completed,
                ..
            }
        ));
    }

    #[gpui::test]
    fn lost_pointer_button_should_cancel_once(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(point(start.x + px(10.0), start.y), None, Modifiers::none());
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());

        assert!(matches!(
            events.borrow().last(),
            Some(ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::PointerButtonLost,
                ..
            })
        ));
        assert_eq!(events.borrow().len(), 2);
    }

    #[gpui::test]
    fn disablement_and_window_deactivation_should_cancel_active_drags(cx: &mut TestAppContext) {
        let (root, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(matches!(
            events.borrow().last(),
            Some(ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::Disabled,
                ..
            })
        ));

        root.update(cx, |root, cx| {
            root.disabled = false;
            cx.notify();
        });
        cx.run_until_parked();
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.deactivate_window();
        assert!(matches!(
            events.borrow().last(),
            Some(ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::WindowDeactivated,
                ..
            })
        ));
    }

    #[gpui::test]
    fn escape_should_request_restoration_before_finishing(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(start.x + px(20.0), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_keystrokes("escape");

        let events = events.borrow();
        assert!(matches!(
            events[2],
            ResizeHandleEvent::ResizeRequested {
                displacement: 0.0,
                requested_value: 100.0,
                ..
            }
        ));
        assert!(matches!(
            events[3],
            ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::Escape,
                ..
            }
        ));
    }

    #[gpui::test]
    fn keyboard_should_use_axis_keys_and_shift_step(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        cx.update(|window, _| window.focus_next());
        cx.simulate_keystrokes("right shift-left down");

        let requests = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ResizeHandleEvent::ResizeRequested {
                    displacement,
                    requested_value,
                    ..
                } => Some((*displacement, *requested_value)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(requests, [(1.0, 101.0), (-10.0, 91.0)]);
    }

    #[gpui::test]
    fn repeated_keyboard_input_should_accumulate_before_the_next_render(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        cx.update(|window, _| window.focus_next());

        cx.simulate_keystrokes("right right right");

        let requests = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ResizeHandleEvent::ResizeRequested {
                    requested_value, ..
                } => Some(*requested_value),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(requests, [101.0, 102.0, 103.0]);
    }

    #[gpui::test]
    fn keyboard_resize_should_be_rejected_while_pointer_capture_is_active(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());

        cx.simulate_keystrokes("right");
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());

        let events = events.borrow();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0],
            ResizeHandleEvent::InteractionStarted {
                source: ResizeInputSource::Pointer,
                ..
            }
        ));
        assert!(matches!(
            events[1],
            ResizeHandleEvent::InteractionFinished {
                source: ResizeInputSource::Pointer,
                reason: ResizeFinishReason::Completed,
                ..
            }
        ));
    }

    #[gpui::test]
    fn disabling_focused_handle_should_release_responder_focus(cx: &mut TestAppContext) {
        let (root, _, cx) = resize_window(cx, ResizeAxis::Horizontal);
        cx.update(|window, _| window.focus_next());
        assert!(cx.update(|window, cx| window.focused(cx).is_some()));

        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn double_click_should_request_reset_without_starting_drag(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let center = hitbox(cx).center();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: center,
            modifiers: Modifiers::none(),
            click_count: 2,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: center,
            modifiers: Modifiers::none(),
            click_count: 2,
        });

        assert_eq!(
            events.borrow().as_slice(),
            [ResizeHandleEvent::ResetRequested {
                source: ResizeInputSource::Pointer
            }]
        );
    }

    #[gpui::test]
    fn pointer_press_in_invisible_hitbox_margin_should_start_interaction(cx: &mut TestAppContext) {
        let (_, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let divider = cx
            .debug_bounds("test-resize-divider")
            .expect("divider was rendered");
        let target = hitbox(cx);
        let margin = point(target.right() - px(0.5), target.center().y);
        assert!(!divider.contains(&margin));

        cx.simulate_mouse_down(margin, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(margin, MouseButton::Left, Modifiers::none());

        assert!(matches!(
            events.borrow().first(),
            Some(ResizeHandleEvent::InteractionStarted { .. })
        ));
    }

    #[gpui::test]
    fn removing_handle_during_drag_should_finish_owned_interaction(cx: &mut TestAppContext) {
        let (root, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        root.update(cx, |root, cx| {
            root.show = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();

        assert!(matches!(
            events.borrow().last(),
            Some(ResizeHandleEvent::InteractionFinished {
                reason: ResizeFinishReason::HandleRemoved,
                ..
            })
        ));
    }

    #[gpui::test]
    fn caller_clamping_should_not_rebase_cumulative_drag_displacement(cx: &mut TestAppContext) {
        let (root, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        root.update(cx, |root, cx| {
            root.clamp = Some((90.0, 110.0));
            cx.notify();
        });
        cx.run_until_parked();
        let start = hitbox(cx).center();
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(start.x + px(30.0), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
        cx.simulate_mouse_move(
            point(start.x - px(10.0), start.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(start, MouseButton::Left, Modifiers::none());

        let requests = events
            .borrow()
            .iter()
            .filter_map(|event| match event {
                ResizeHandleEvent::ResizeRequested {
                    requested_value, ..
                } => Some(*requested_value),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(requests, [130.0, 90.0]);
        assert_eq!(root.read_with(cx, |root, _| root.value), 90.0);
    }

    #[gpui::test]
    fn tab_stop_should_control_keyboard_focus_participation(cx: &mut TestAppContext) {
        let (root, events, cx) = resize_window(cx, ResizeAxis::Horizontal);
        root.update(cx, |root, cx| {
            root.tab_stop = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| window.focus_next());
        cx.simulate_keystrokes("right");
        assert!(events.borrow().is_empty());

        root.update(cx, |root, cx| {
            root.tab_stop = true;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, _| window.focus_next());
        cx.simulate_keystrokes("right");
        assert!(
            events
                .borrow()
                .iter()
                .any(|event| matches!(event, ResizeHandleEvent::ResizeRequested { .. }))
        );
    }

    #[gpui::test]
    fn multiple_handles_should_keep_interactions_independent(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        let first_events = Rc::new(RefCell::new(Vec::new()));
        let second_events = Rc::new(RefCell::new(Vec::new()));
        let root_first = Rc::clone(&first_events);
        let root_second = Rc::clone(&second_events);
        let (_, cx) = cx.add_window_view(move |_, _| MultipleRoot {
            first_events: root_first,
            second_events: root_second,
        });
        cx.run_until_parked();
        let second = cx
            .debug_bounds("second-resize-hitbox")
            .expect("second handle was rendered")
            .center();
        cx.simulate_mouse_down(second, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(second.x + px(15.0), second.y),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.simulate_mouse_up(second, MouseButton::Left, Modifiers::none());

        assert!(first_events.borrow().is_empty());
        assert!(second_events.borrow().iter().any(|event| matches!(
            event,
            ResizeHandleEvent::ResizeRequested {
                requested_value: 135.0,
                ..
            }
        )));
    }

    #[gpui::test]
    fn hitbox_should_be_larger_than_visible_divider(cx: &mut TestAppContext) {
        let (_, _, cx) = resize_window(cx, ResizeAxis::Horizontal);
        let root = cx.debug_bounds("test-resize").expect("root was rendered");
        let target = hitbox(cx);
        let divider = cx
            .debug_bounds("test-resize-divider")
            .expect("divider was rendered");

        assert_eq!(root.size.width, px(9.0));
        assert_eq!(target.size.width, px(9.0));
        assert_eq!(divider.size.width, px(1.0));
    }
}
