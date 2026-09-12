use std::rc::Rc;

use gpui::{
    App, ClickEvent, ElementId, FocusHandle, Global, InteractiveElement as _, IntoElement,
    KeyDownEvent, KeyUpEvent, MouseButton, ParentElement as _, Pixels, RenderOnce, Rgba,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, div,
    prelude::FluentBuilder as _, px,
};

use crate::tooltip::{Tooltip, TooltipTargetVisibility};

const INTERACTION_GROUP: &str = "spaceterm-toggle";

/// The state represented by a checkbox.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CheckboxState {
    /// The option is not selected.
    #[default]
    Unchecked,
    /// The option is selected.
    Checked,
    /// A governed collection contains both selected and unselected values.
    Mixed,
}

impl CheckboxState {
    /// Returns the state requested by ordinary user activation.
    ///
    /// Mixed is a derived collection state, so activating it requests Checked rather than making
    /// callers expose Mixed as a third user-selectable value.
    pub const fn after_activation(self) -> Self {
        match self {
            Self::Unchecked | Self::Mixed => Self::Checked,
            Self::Checked => Self::Unchecked,
        }
    }
}

/// The input path that requested a toggle state change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToggleActivationSource {
    /// A primary pointer press released inside the complete labeled control.
    Pointer,
    /// An unmodified Space key press released while the control retained focus.
    Space,
}

/// A controlled checkbox change request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckboxChange {
    previous: CheckboxState,
    requested: CheckboxState,
    source: ToggleActivationSource,
}

impl CheckboxChange {
    /// Returns the caller-owned state rendered when activation began.
    pub const fn previous(self) -> CheckboxState {
        self.previous
    }

    /// Returns the next state requested by the user.
    pub const fn requested(self) -> CheckboxState {
        self.requested
    }

    /// Returns the input path that requested the change.
    pub const fn source(self) -> ToggleActivationSource {
        self.source
    }
}

/// A controlled switch change request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SwitchChange {
    previous: bool,
    requested: bool,
    source: ToggleActivationSource,
}

impl SwitchChange {
    /// Returns whether the switch was on when activation began.
    pub const fn previous(self) -> bool {
        self.previous
    }

    /// Returns the next on/off value requested by the user.
    pub const fn requested(self) -> bool {
        self.requested
    }

    /// Returns the input path that requested the change.
    pub const fn source(self) -> ToggleActivationSource {
        self.source
    }
}

/// Standard sizes for checkbox and switch controls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToggleSize {
    /// Dense controls embedded in compact settings rows.
    Compact,
    /// Ordinary controls in forms and settings surfaces.
    #[default]
    Regular,
}

/// Paint for one value in one interaction state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TogglePaint {
    background: Rgba,
    foreground: Rgba,
    border: Rgba,
    label: Rgba,
}

impl TogglePaint {
    /// Creates resolved indicator and label paint.
    pub const fn new(background: Rgba, foreground: Rgba, border: Rgba, label: Rgba) -> Self {
        Self {
            background,
            foreground,
            border,
            label,
        }
    }

    /// Returns the checkbox box or switch track fill.
    pub const fn background(self) -> Rgba {
        self.background
    }

    /// Returns the check mark, mixed mark, or switch thumb fill.
    pub const fn foreground(self) -> Rgba {
        self.foreground
    }

    /// Returns the checkbox box or switch track border.
    pub const fn border(self) -> Rgba {
        self.border
    }

    /// Returns the visible label color.
    pub const fn label(self) -> Rgba {
        self.label
    }
}

/// Paint for off and on values in one interaction state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToggleValuePaints {
    off: TogglePaint,
    on: TogglePaint,
}

impl ToggleValuePaints {
    /// Creates value-specific paint. A mixed checkbox uses the on paint and a distinct mark.
    pub const fn new(off: TogglePaint, on: TogglePaint) -> Self {
        Self { off, on }
    }

    fn resolve(self, on: bool) -> TogglePaint {
        if on { self.on } else { self.off }
    }
}

/// Complete interaction-state paint for the toggle family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TogglePaints {
    normal: ToggleValuePaints,
    hovered: ToggleValuePaints,
    pressed: ToggleValuePaints,
    disabled: ToggleValuePaints,
}

impl TogglePaints {
    /// Creates the complete bounded paint catalog.
    pub const fn new(
        normal: ToggleValuePaints,
        hovered: ToggleValuePaints,
        pressed: ToggleValuePaints,
        disabled: ToggleValuePaints,
    ) -> Self {
        Self {
            normal,
            hovered,
            pressed,
            disabled,
        }
    }
}

/// Geometry and typography for one standard toggle size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToggleMetrics {
    row_height: Pixels,
    checkbox_extent: Pixels,
    switch_width: Pixels,
    switch_height: Pixels,
    label_gap: Pixels,
    checkbox_radius: Pixels,
    switch_inset: Pixels,
    border_width: Pixels,
    focus_gap: Pixels,
    font_size: Pixels,
    line_height: f32,
}

impl ToggleMetrics {
    /// Creates compact desktop geometry for both representations.
    pub fn new(
        row_height: Pixels,
        checkbox_extent: Pixels,
        switch_width: Pixels,
        switch_height: Pixels,
    ) -> Self {
        Self {
            row_height,
            checkbox_extent,
            switch_width,
            switch_height,
            label_gap: px(8.0),
            checkbox_radius: px(3.0),
            switch_inset: px(2.0),
            border_width: px(1.0),
            focus_gap: px(2.0),
            font_size: px(12.0),
            line_height: 1.2,
        }
    }

    /// Sets the spacing between the indicator and visible label.
    pub fn label_gap(mut self, gap: Pixels) -> Self {
        self.label_gap = gap;
        self
    }

    /// Sets checkbox corner rounding independently from the pill-shaped switch.
    pub fn checkbox_radius(mut self, radius: Pixels) -> Self {
        self.checkbox_radius = radius;
        self
    }

    /// Sets the switch thumb inset from the inside of its track.
    pub fn switch_inset(mut self, inset: Pixels) -> Self {
        self.switch_inset = inset;
        self
    }

    /// Sets the stable indicator border width used in every visual state.
    pub fn border_width(mut self, width: Pixels) -> Self {
        self.border_width = width;
        self
    }

    /// Sets the space between an indicator and its keyboard focus outline.
    pub fn focus_gap(mut self, gap: Pixels) -> Self {
        self.focus_gap = gap;
        self
    }

    /// Sets visible-label typography.
    pub fn typography(mut self, font_size: Pixels, line_height: f32) -> Self {
        self.font_size = font_size;
        self.line_height = line_height.clamp(1.0, 2.0);
        self
    }

    fn scaled(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            row_height: crate::appearance::scale_line_box(
                self.row_height,
                self.font_size,
                text_scale,
                spacing_scale,
            ),
            checkbox_extent: crate::appearance::scale_metric(self.checkbox_extent, spacing_scale),
            switch_width: crate::appearance::scale_metric(self.switch_width, spacing_scale),
            switch_height: crate::appearance::scale_metric(self.switch_height, spacing_scale),
            label_gap: crate::appearance::scale_metric(self.label_gap, spacing_scale),
            checkbox_radius: crate::appearance::scale_metric(self.checkbox_radius, spacing_scale),
            switch_inset: crate::appearance::scale_metric(self.switch_inset, spacing_scale),
            border_width: self.border_width,
            focus_gap: crate::appearance::scale_metric(self.focus_gap, spacing_scale),
            font_size: crate::appearance::scale_metric(self.font_size, text_scale),
            line_height: self.line_height,
        }
    }
}

/// Complete metrics for the toggle family's standard sizes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToggleSizes {
    compact: ToggleMetrics,
    regular: ToggleMetrics,
}

impl ToggleSizes {
    /// Creates a complete size catalog.
    pub const fn new(compact: ToggleMetrics, regular: ToggleMetrics) -> Self {
        Self { compact, regular }
    }

    fn resolve(self, size: ToggleSize) -> ToggleMetrics {
        match size {
            ToggleSize::Compact => self.compact,
            ToggleSize::Regular => self.regular,
        }
    }

    fn scaled(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            compact: self.compact.scaled(text_scale, spacing_scale),
            regular: self.regular.scaled(text_scale, spacing_scale),
        }
    }
}

/// Application-owned presentation for checkboxes and switches.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToggleTheme {
    paints: TogglePaints,
    sizes: ToggleSizes,
    focus_border: Rgba,
}

impl ToggleTheme {
    /// Creates a complete resolved toggle theme.
    pub const fn new(paints: TogglePaints, sizes: ToggleSizes, focus_border: Rgba) -> Self {
        Self {
            paints,
            sizes,
            focus_border,
        }
    }

    /// Returns a copy with text and spacing metrics scaled independently.
    pub fn scaled_metrics(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            sizes: self.sizes.scaled(text_scale, spacing_scale),
            ..self
        }
    }

    fn resolve(self, size: ToggleSize, on: bool) -> ToggleStyle {
        ToggleStyle {
            normal: self.paints.normal.resolve(on),
            hovered: self.paints.hovered.resolve(on),
            pressed: self.paints.pressed.resolve(on),
            disabled: self.paints.disabled.resolve(on),
            metrics: self.sizes.resolve(size),
            focus_border: self.focus_border,
        }
    }
}

impl Global for ToggleTheme {}

/// A labeled checkbox with controlled two-state or derived mixed-state semantics.
///
/// The visible label is also retained as the logical accessibility name. GPUI 0.2.2 cannot yet
/// publish custom checkbox roles and checked state to the native accessibility tree, so callers do
/// not need to retrofit a different public interface when that framework seam becomes available.
#[derive(IntoElement)]
pub struct Checkbox {
    core: ToggleCore,
    state: CheckboxState,
}

impl Checkbox {
    /// Creates a regular checkbox. The complete labeled row is its hit target and a Tab stop.
    pub fn new(
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        state: CheckboxState,
    ) -> Self {
        Self {
            core: ToggleCore::new(id.into(), label.into()),
            state,
        }
    }

    /// Selects a standard desktop control size.
    pub fn size(mut self, size: ToggleSize) -> Self {
        self.core.size = size;
        self
    }

    /// Controls whether the checkbox can request a state change.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.core.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on the checkbox.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.core.tab_stop = tab_stop;
        self
    }

    /// Hides the visible label while retaining it as the logical accessibility name.
    ///
    /// Use it only where surrounding presentation already names the control, such as a settings
    /// row whose own label column carries the name.
    pub fn label_hidden(mut self, label_hidden: bool) -> Self {
        self.core.label_hidden = label_hidden;
        self
    }

    /// Makes the labeled hit target fill the available width.
    pub fn full_width(mut self, full_width: bool) -> Self {
        self.core.full_width = full_width;
        self
    }

    /// Mirrors indicator placement for a right-to-left surrounding layout.
    pub fn right_to_left(mut self, right_to_left: bool) -> Self {
        self.core.right_to_left = right_to_left;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.core.debug_selector = Some(selector.into());
        self
    }

    /// Attaches bounded semantic tooltip content.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.core.tooltip = Some(tooltip);
        self
    }

    /// Handles a requested state change. The caller remains the state authority.
    pub fn on_change(
        mut self,
        handler: impl Fn(&CheckboxChange, &mut Window, &mut App) + 'static,
    ) -> Self {
        let previous = self.state;
        self.core.on_activate = Some(Rc::new(move |source, window, cx| {
            handler(
                &CheckboxChange {
                    previous,
                    requested: previous.after_activation(),
                    source,
                },
                window,
                cx,
            );
        }));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state;
        self.core.render(
            ToggleKind::Checkbox(state),
            state != CheckboxState::Unchecked,
            window,
            cx,
        )
    }
}

/// A labeled binary switch whose requested changes are intended to take effect immediately.
///
/// A switch never accepts a mixed value and its visible label remains stable across state changes.
/// GPUI 0.2.2 cannot yet publish a custom switch role and checked state to the native
/// accessibility tree; the mandatory logical label and typed value preserve that semantic seam.
#[derive(IntoElement)]
pub struct Switch {
    core: ToggleCore,
    on: bool,
}

impl Switch {
    /// Creates a regular binary switch. The complete labeled row is its hit target and a Tab stop.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>, on: bool) -> Self {
        Self {
            core: ToggleCore::new(id.into(), label.into()),
            on,
        }
    }

    /// Selects a standard desktop control size.
    pub fn size(mut self, size: ToggleSize) -> Self {
        self.core.size = size;
        self
    }

    /// Controls whether the switch can request a state change.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.core.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on the switch.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.core.tab_stop = tab_stop;
        self
    }

    /// Hides the visible label while retaining it as the logical accessibility name.
    ///
    /// Use it only where surrounding presentation already names the control, such as a settings
    /// row whose own label column carries the name.
    pub fn label_hidden(mut self, label_hidden: bool) -> Self {
        self.core.label_hidden = label_hidden;
        self
    }

    /// Makes the label and trailing switch fill the available width.
    pub fn full_width(mut self, full_width: bool) -> Self {
        self.core.full_width = full_width;
        self
    }

    /// Mirrors label placement and the physical on/off thumb direction.
    pub fn right_to_left(mut self, right_to_left: bool) -> Self {
        self.core.right_to_left = right_to_left;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.core.debug_selector = Some(selector.into());
        self
    }

    /// Attaches bounded semantic tooltip content.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.core.tooltip = Some(tooltip);
        self
    }

    /// Handles a requested immediate on/off change. The caller remains the state authority.
    pub fn on_change(
        mut self,
        handler: impl Fn(&SwitchChange, &mut Window, &mut App) + 'static,
    ) -> Self {
        let previous = self.on;
        self.core.on_activate = Some(Rc::new(move |source, window, cx| {
            handler(
                &SwitchChange {
                    previous,
                    requested: !previous,
                    source,
                },
                window,
                cx,
            );
        }));
        self
    }
}

impl RenderOnce for Switch {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        self.core.render(ToggleKind::Switch, self.on, window, cx)
    }
}

type ToggleActivationHandler = Rc<dyn Fn(ToggleActivationSource, &mut Window, &mut App)>;

#[derive(Clone, Copy)]
enum ToggleKind {
    Checkbox(CheckboxState),
    Switch,
}

struct ToggleCore {
    id: ElementId,
    label: SharedString,
    label_hidden: bool,
    size: ToggleSize,
    disabled: bool,
    tab_stop: bool,
    full_width: bool,
    right_to_left: bool,
    debug_selector: Option<String>,
    tooltip: Option<Tooltip>,
    on_activate: Option<ToggleActivationHandler>,
}

impl ToggleCore {
    fn new(id: ElementId, label: SharedString) -> Self {
        Self {
            id,
            label,
            label_hidden: false,
            size: ToggleSize::default(),
            disabled: false,
            tab_stop: true,
            full_width: false,
            right_to_left: false,
            debug_selector: None,
            tooltip: None,
            on_activate: None,
        }
    }

    fn render(
        self,
        kind: ToggleKind,
        on: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let style = cx.global::<ToggleTheme>().resolve(self.size, on);
        let enabled = !self.disabled && self.on_activate.is_some();
        let state = window.use_keyed_state(self.id.clone(), cx, |window, cx| {
            ToggleControlState::new(window, cx)
        });
        let focus_handle = state.read(cx).focus_handle.clone();
        if !enabled && focus_handle.is_focused(window) {
            window.blur();
        }
        state.update(cx, |state, cx| {
            state.synchronize(enabled, self.tab_stop, cx);
        });
        let (keyboard_pressed, focus_visible) =
            state.read_with(cx, |state, _| (state.keyboard_pressed, state.focus_visible));
        let focused = focus_handle.is_focused(window) && focus_visible;
        let paint = if !enabled {
            style.disabled
        } else if keyboard_pressed {
            style.pressed
        } else {
            style.normal
        };
        let selector = self
            .debug_selector
            .unwrap_or_else(|| self.label.to_string());
        let indicator_selector = format!("{selector}-indicator");
        let thumb_selector = format!("{selector}-thumb");
        let focus_selector = format!("{selector}-keyboard-focus");
        let label_state_id = format!("{selector}-label-state");
        let indicator = match kind {
            ToggleKind::Checkbox(value) => checkbox_indicator(
                value,
                style,
                paint,
                enabled,
                keyboard_pressed,
                focused,
                indicator_selector,
                focus_selector,
            ),
            ToggleKind::Switch => switch_indicator(
                on,
                self.right_to_left,
                style,
                paint,
                enabled,
                keyboard_pressed,
                focused,
                indicator_selector,
                thumb_selector,
                focus_selector,
            ),
        };
        let hovered = TogglePaintRefinement(style.hovered);
        let pressed = TogglePaintRefinement(style.pressed);
        let label = div()
            .id(SharedString::from(label_state_id))
            .min_w_0()
            .text_color(paint.label)
            .text_size(style.metrics.font_size)
            .line_height(gpui::relative(style.metrics.line_height))
            .font(crate::control_typography(cx).regular().clone())
            .child(self.label.clone())
            .when(enabled && !keyboard_pressed, |label| {
                label
                    .group_hover(INTERACTION_GROUP, move |style| hovered.label(style))
                    .group_active(INTERACTION_GROUP, move |style| pressed.label(style))
            });
        let is_switch = matches!(kind, ToggleKind::Switch);
        let content = if self.label_hidden {
            vec![indicator]
        } else if is_switch {
            vec![label.into_any_element(), indicator]
        } else {
            vec![indicator, label.into_any_element()]
        };
        let on_activate = self.on_activate.clone();
        let click_handler = move |event: &ClickEvent, window: &mut Window, cx: &mut App| {
            if !matches!(event, ClickEvent::Mouse(_)) {
                return;
            }
            window.prevent_default();
            if let Some(handler) = &on_activate {
                handler(ToggleActivationSource::Pointer, window, cx);
            }
            cx.stop_propagation();
        };
        let pointer_state = state.clone();
        let key_down_state = state.clone();
        let key_up_state = state;
        let keyboard_focus = focus_handle.clone();
        let keyboard_handler = self.on_activate;
        let row = div()
            .id(self.id)
            .debug_selector(move || selector)
            .relative()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .gap(style.metrics.label_gap)
            .min_h(style.metrics.row_height)
            .when(self.full_width, |row| row.w_full())
            .when(self.full_width && is_switch, |row| row.justify_between())
            .when(self.right_to_left, |row| row.flex_row_reverse())
            .cursor_default()
            .block_mouse_except_scroll()
            .when(enabled, |row| {
                row.track_focus(&focus_handle)
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        pointer_state.update(cx, |state, cx| state.pointer_focus(cx));
                    })
                    .on_key_down(move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.key != "space" || event.keystroke.modifiers.modified() {
                            return;
                        }
                        window.prevent_default();
                        if !event.is_held {
                            key_down_state.update(cx, |state, cx| state.keyboard_down(cx));
                        }
                        cx.stop_propagation();
                    })
                    .on_key_up(move |event: &KeyUpEvent, window, cx| {
                        if event.keystroke.key != "space" || !key_up_state.read(cx).keyboard_pressed
                        {
                            return;
                        }
                        let may_activate = !event.keystroke.modifiers.modified()
                            && keyboard_focus.is_focused(window);
                        let activate = key_up_state
                            .update(cx, |state, cx| state.keyboard_up(may_activate, cx));
                        if activate && let Some(handler) = &keyboard_handler {
                            handler(ToggleActivationSource::Space, window, cx);
                        }
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .group(INTERACTION_GROUP)
                    .on_click(click_handler)
            })
            .children(content);

        if let Some(tooltip) = self.tooltip {
            tooltip
                .attach(row, TooltipTargetVisibility::Visible)
                .disabled(!enabled)
                .into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

#[derive(Clone, Copy)]
struct ToggleStyle {
    normal: TogglePaint,
    hovered: TogglePaint,
    pressed: TogglePaint,
    disabled: TogglePaint,
    metrics: ToggleMetrics,
    focus_border: Rgba,
}

#[derive(Clone, Copy)]
struct TogglePaintRefinement(TogglePaint);

impl TogglePaintRefinement {
    fn indicator(self, style: StyleRefinement) -> StyleRefinement {
        style.bg(self.0.background).border_color(self.0.border)
    }

    fn foreground_fill(self, style: StyleRefinement) -> StyleRefinement {
        style.bg(self.0.foreground)
    }

    fn foreground_text(self, style: StyleRefinement) -> StyleRefinement {
        style.text_color(self.0.foreground)
    }

    fn label(self, style: StyleRefinement) -> StyleRefinement {
        style.text_color(self.0.label)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the indicator consumes one resolved control state"
)]
fn checkbox_indicator(
    value: CheckboxState,
    style: ToggleStyle,
    paint: TogglePaint,
    enabled: bool,
    keyboard_pressed: bool,
    focused: bool,
    selector: String,
    focus_selector: String,
) -> gpui::AnyElement {
    let metrics = style.metrics;
    let hovered = TogglePaintRefinement(style.hovered);
    let pressed = TogglePaintRefinement(style.pressed);
    let state_id = format!("{selector}-state");
    let mark_state_id = SharedString::from(format!("{selector}-mark-state"));
    let indicator = div()
        .id(SharedString::from(state_id))
        .debug_selector(move || selector)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(metrics.checkbox_extent)
        .rounded(metrics.checkbox_radius)
        .border(metrics.border_width)
        .border_color(paint.border)
        .bg(paint.background)
        .when(enabled && !keyboard_pressed, |indicator| {
            indicator
                .group_hover(INTERACTION_GROUP, move |style| hovered.indicator(style))
                .group_active(INTERACTION_GROUP, move |style| pressed.indicator(style))
        })
        .when(value == CheckboxState::Checked, |indicator| {
            indicator.child(
                div()
                    .id(mark_state_id.clone())
                    .text_color(paint.foreground)
                    .text_size(metrics.checkbox_extent * 0.8)
                    .line_height(metrics.checkbox_extent)
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("✓")
                    .when(enabled && !keyboard_pressed, |mark| {
                        mark.group_hover(INTERACTION_GROUP, move |style| {
                            hovered.foreground_text(style)
                        })
                        .group_active(INTERACTION_GROUP, move |style| {
                            pressed.foreground_text(style)
                        })
                    }),
            )
        })
        .when(value == CheckboxState::Mixed, |indicator| {
            indicator.child(
                div()
                    .id(mark_state_id)
                    .w(metrics.checkbox_extent * 0.5)
                    .h(metrics.border_width * 2.0)
                    .rounded(metrics.border_width)
                    .bg(paint.foreground)
                    .when(enabled && !keyboard_pressed, |mark| {
                        mark.group_hover(INTERACTION_GROUP, move |style| {
                            hovered.foreground_fill(style)
                        })
                        .group_active(INTERACTION_GROUP, move |style| {
                            pressed.foreground_fill(style)
                        })
                    }),
            )
        })
        .when(focused, |indicator| {
            indicator.child(focus_outline(
                metrics.checkbox_radius,
                metrics,
                style.focus_border,
                focus_selector,
            ))
        });
    indicator.into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the indicator consumes one resolved control state"
)]
fn switch_indicator(
    on: bool,
    right_to_left: bool,
    style: ToggleStyle,
    paint: TogglePaint,
    enabled: bool,
    keyboard_pressed: bool,
    focused: bool,
    selector: String,
    thumb_selector: String,
    focus_selector: String,
) -> gpui::AnyElement {
    let metrics = style.metrics;
    let hovered = TogglePaintRefinement(style.hovered);
    let pressed = TogglePaintRefinement(style.pressed);
    let state_id = format!("{selector}-state");
    let thumb_state_id = SharedString::from(format!("{thumb_selector}-state"));
    let thumb_extent = (metrics.switch_height - metrics.switch_inset * 2.0).max(px(1.0));
    let content_inset = (metrics.switch_inset - metrics.border_width).max(px(0.0));
    let off_offset = px(0.0);
    let on_offset =
        (metrics.switch_width - metrics.switch_inset * 2.0 - thumb_extent).max(off_offset);
    let thumb_offset = if on != right_to_left {
        on_offset
    } else {
        off_offset
    };
    let radius = metrics.switch_height / 2.0;
    let indicator = div()
        .id(SharedString::from(state_id))
        .debug_selector(move || selector)
        .relative()
        .flex()
        .items_center()
        .flex_none()
        .w(metrics.switch_width)
        .h(metrics.switch_height)
        .p(content_inset)
        .rounded(radius)
        .border(metrics.border_width)
        .border_color(paint.border)
        .bg(paint.background)
        .when(enabled && !keyboard_pressed, |indicator| {
            indicator
                .group_hover(INTERACTION_GROUP, move |style| hovered.indicator(style))
                .group_active(INTERACTION_GROUP, move |style| pressed.indicator(style))
        })
        .child(
            div()
                .id(thumb_state_id)
                .debug_selector(move || thumb_selector)
                .relative()
                .left(thumb_offset)
                .size(thumb_extent)
                .rounded(thumb_extent / 2.0)
                .bg(paint.foreground)
                .when(enabled && !keyboard_pressed, |thumb| {
                    thumb
                        .group_hover(INTERACTION_GROUP, move |style| {
                            hovered.foreground_fill(style)
                        })
                        .group_active(INTERACTION_GROUP, move |style| {
                            pressed.foreground_fill(style)
                        })
                }),
        )
        .when(focused, |indicator| {
            indicator.child(focus_outline(
                radius,
                metrics,
                style.focus_border,
                focus_selector,
            ))
        });
    indicator.into_any_element()
}

fn focus_outline(
    radius: Pixels,
    metrics: ToggleMetrics,
    color: Rgba,
    selector: String,
) -> impl IntoElement {
    let offset = metrics.focus_gap + metrics.border_width;
    div()
        .debug_selector(move || selector)
        .absolute()
        .top(-offset)
        .right(-offset)
        .bottom(-offset)
        .left(-offset)
        .rounded(radius + metrics.focus_gap)
        .border(metrics.border_width)
        .border_color(color)
}

struct ToggleControlState {
    focus_handle: FocusHandle,
    enabled: bool,
    keyboard_pressed: bool,
    focus_visible: bool,
}

impl ToggleControlState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&focus_handle, window, |state, _, cx| {
            state.keyboard_pressed = false;
            state.focus_visible = true;
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |state, window, cx| {
            if !window.is_window_active() && state.keyboard_pressed {
                state.keyboard_pressed = false;
                cx.notify();
            }
        })
        .detach();
        Self {
            focus_handle,
            enabled: false,
            keyboard_pressed: false,
            focus_visible: true,
        }
    }

    fn synchronize(&mut self, enabled: bool, tab_stop: bool, cx: &mut gpui::Context<Self>) {
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && tab_stop);
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled && self.keyboard_pressed {
                self.keyboard_pressed = false;
                cx.notify();
            }
        }
    }

    fn keyboard_down(&mut self, cx: &mut gpui::Context<Self>) {
        if self.enabled && (!self.keyboard_pressed || !self.focus_visible) {
            self.keyboard_pressed = true;
            self.focus_visible = true;
            cx.notify();
        }
    }

    fn pointer_focus(&mut self, cx: &mut gpui::Context<Self>) {
        if self.focus_visible {
            self.focus_visible = false;
            cx.notify();
        }
    }

    fn keyboard_up(&mut self, focused: bool, cx: &mut gpui::Context<Self>) -> bool {
        let activate = self.enabled && focused && self.keyboard_pressed;
        if self.keyboard_pressed {
            self.keyboard_pressed = false;
            cx.notify();
        }
        activate
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Context, Entity, FocusHandle, Keystroke, Modifiers, Render, TestAppContext,
        VisualTestContext, Window, point, rgba,
    };

    use super::*;

    fn test_theme() -> ToggleTheme {
        let normal = ToggleValuePaints::new(
            TogglePaint::new(
                rgba(0x202020ff),
                rgba(0xffffffff),
                rgba(0x808080ff),
                rgba(0xffffffff),
            ),
            TogglePaint::new(
                rgba(0x2277ddff),
                rgba(0xffffffff),
                rgba(0x2277ddff),
                rgba(0xffffffff),
            ),
        );
        let metrics = ToggleMetrics::new(px(24.0), px(16.0), px(34.0), px(18.0));
        ToggleTheme::new(
            TogglePaints::new(normal, normal, normal, normal),
            ToggleSizes::new(metrics, metrics),
            rgba(0x00aaffff),
        )
    }

    #[test]
    fn mixed_checkbox_activation_should_request_checked() {
        assert_eq!(
            CheckboxState::Mixed.after_activation(),
            CheckboxState::Checked
        );
        assert_eq!(
            CheckboxState::Checked.after_activation(),
            CheckboxState::Unchecked
        );
    }

    #[test]
    fn interaction_paint_should_refine_every_rendered_part() {
        let paint = TogglePaint::new(
            rgba(0x111111ff),
            rgba(0x121212ff),
            rgba(0x131313ff),
            rgba(0x141414ff),
        );
        let refinement = TogglePaintRefinement(paint);

        assert_eq!(
            refinement.indicator(StyleRefinement::default()),
            StyleRefinement::default()
                .bg(paint.background)
                .border_color(paint.border)
        );
        assert_eq!(
            refinement.foreground_fill(StyleRefinement::default()),
            StyleRefinement::default().bg(paint.foreground)
        );
        assert_eq!(
            refinement.foreground_text(StyleRefinement::default()),
            StyleRefinement::default().text_color(paint.foreground)
        );
        assert_eq!(
            refinement.label(StyleRefinement::default()),
            StyleRefinement::default().text_color(paint.label)
        );
    }

    struct TestRoot {
        checkbox_state: CheckboxState,
        switch_on: bool,
        checkbox_changes: Rc<Cell<usize>>,
        switch_changes: Rc<Cell<usize>>,
        last_checkbox: Rc<Cell<Option<CheckboxChange>>>,
        last_switch: Rc<Cell<Option<SwitchChange>>>,
        disabled: bool,
        other_focus: FocusHandle,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let checkbox_changes = Rc::clone(&self.checkbox_changes);
            let last_checkbox = Rc::clone(&self.last_checkbox);
            let switch_changes = Rc::clone(&self.switch_changes);
            let last_switch = Rc::clone(&self.last_switch);
            div()
                .flex()
                .flex_col()
                .child(div().track_focus(&self.other_focus).child("Other"))
                .child(
                    Checkbox::new("test-checkbox", "Show pane captions", self.checkbox_state)
                        .disabled(self.disabled)
                        .debug_selector("test-checkbox")
                        .on_change(move |change, _, _| {
                            checkbox_changes.set(checkbox_changes.get() + 1);
                            last_checkbox.set(Some(*change));
                        }),
                )
                .child(
                    Switch::new("test-switch", "Notifications", self.switch_on)
                        .disabled(self.disabled)
                        .debug_selector("test-switch")
                        .on_change(move |change, _, _| {
                            switch_changes.set(switch_changes.get() + 1);
                            last_switch.set(Some(*change));
                        }),
                )
        }
    }

    type ToggleWindow<'a> = (
        Entity<TestRoot>,
        Rc<Cell<usize>>,
        Rc<Cell<usize>>,
        Rc<Cell<Option<CheckboxChange>>>,
        Rc<Cell<Option<SwitchChange>>>,
        &'a mut VisualTestContext,
    );

    fn toggle_window(cx: &mut TestAppContext, disabled: bool) -> ToggleWindow<'_> {
        cx.set_global(test_theme());
        let checkbox_changes = Rc::new(Cell::new(0));
        let switch_changes = Rc::new(Cell::new(0));
        let last_checkbox = Rc::new(Cell::new(None));
        let last_switch = Rc::new(Cell::new(None));
        let root_checkbox_changes = Rc::clone(&checkbox_changes);
        let root_switch_changes = Rc::clone(&switch_changes);
        let root_last_checkbox = Rc::clone(&last_checkbox);
        let root_last_switch = Rc::clone(&last_switch);
        let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
            checkbox_state: CheckboxState::Mixed,
            switch_on: false,
            checkbox_changes: root_checkbox_changes,
            switch_changes: root_switch_changes,
            last_checkbox: root_last_checkbox,
            last_switch: root_last_switch,
            disabled,
            other_focus: cx.focus_handle().tab_stop(true),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (
            root,
            checkbox_changes,
            switch_changes,
            last_checkbox,
            last_switch,
            cx,
        )
    }

    #[gpui::test]
    fn clicking_complete_checkbox_row_should_request_checked_once(cx: &mut TestAppContext) {
        let (_, changes, _, last, _, cx) = toggle_window(cx, false);
        let bounds = cx
            .debug_bounds("test-checkbox")
            .expect("checkbox should render");

        cx.simulate_click(bounds.center(), Modifiers::none());

        assert_eq!(changes.get(), 1);
        assert_eq!(
            last.get(),
            Some(CheckboxChange {
                previous: CheckboxState::Mixed,
                requested: CheckboxState::Checked,
                source: ToggleActivationSource::Pointer,
            })
        );
    }

    #[gpui::test]
    fn pointer_release_outside_should_not_request_change(cx: &mut TestAppContext) {
        let (_, changes, _, _, _, cx) = toggle_window(cx, false);
        let bounds = cx
            .debug_bounds("test-checkbox")
            .expect("checkbox should render");
        let outside = point(bounds.right() + px(20.0), bounds.bottom() + px(20.0));

        cx.simulate_mouse_down(bounds.center(), gpui::MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(outside, gpui::MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_up(outside, gpui::MouseButton::Left, Modifiers::none());

        assert_eq!(changes.get(), 0);
    }

    #[gpui::test]
    fn focused_space_should_request_switch_change_on_key_up(cx: &mut TestAppContext) {
        let (_, _, changes, _, last, cx) = toggle_window(cx, false);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
            window.focus_next();
        });
        let space = Keystroke::parse("space").expect("space should parse");

        cx.simulate_event(KeyDownEvent {
            keystroke: space.clone(),
            is_held: false,
        });
        assert_eq!(changes.get(), 0);
        cx.simulate_event(KeyUpEvent { keystroke: space });

        assert_eq!(changes.get(), 1);
        assert_eq!(
            last.get(),
            Some(SwitchChange {
                previous: false,
                requested: true,
                source: ToggleActivationSource::Space,
            })
        );
    }

    #[gpui::test]
    fn modified_space_release_should_cancel_without_requesting_change(cx: &mut TestAppContext) {
        let (_, checkbox_changes, _, _, _, cx) = toggle_window(cx, false);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });
        let space = Keystroke::parse("space").expect("space should parse");
        let shifted_space = Keystroke::parse("shift-space").expect("shift-space should parse");

        cx.simulate_event(KeyDownEvent {
            keystroke: space.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent {
            keystroke: shifted_space,
        });
        cx.simulate_event(KeyUpEvent { keystroke: space });

        assert_eq!(checkbox_changes.get(), 0);
    }

    #[gpui::test]
    fn enter_should_not_change_checkbox_or_switch(cx: &mut TestAppContext) {
        let (_, checkbox_changes, switch_changes, _, _, cx) = toggle_window(cx, false);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });
        let enter = Keystroke::parse("enter").expect("enter should parse");

        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke: enter });

        assert_eq!(checkbox_changes.get(), 0);
        assert_eq!(switch_changes.get(), 0);
    }

    #[gpui::test]
    fn disabled_toggles_should_skip_tab_order_and_ignore_pointer(cx: &mut TestAppContext) {
        let (root, checkbox_changes, switch_changes, _, _, cx) = toggle_window(cx, true);
        let other = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, _| {
            window.focus_next();
        });
        assert!(cx.update(|window, _| other.is_focused(window)));

        for selector in ["test-checkbox", "test-switch"] {
            let bounds = cx.debug_bounds(selector).expect("toggle should render");
            cx.simulate_click(bounds.center(), Modifiers::none());
        }

        assert_eq!(checkbox_changes.get(), 0);
        assert_eq!(switch_changes.get(), 0);
    }

    #[gpui::test]
    fn disabled_toggle_pointer_should_preserve_existing_focus(cx: &mut TestAppContext) {
        let (root, _, _, _, _, cx) = toggle_window(cx, true);
        let other = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, _| other.focus(window));
        let bounds = cx
            .debug_bounds("test-checkbox")
            .expect("checkbox should render");

        cx.simulate_click(bounds.center(), Modifiers::none());

        assert!(cx.update(|window, _| other.is_focused(window)));
    }

    #[gpui::test]
    fn disabling_focused_toggle_should_release_focus(cx: &mut TestAppContext) {
        let (root, _, _, _, _, cx) = toggle_window(cx, false);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });

        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn keyboard_focus_should_draw_one_outset_indicator_ring(cx: &mut TestAppContext) {
        let (_, _, _, _, _, cx) = toggle_window(cx, false);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });
        cx.run_until_parked();

        let indicator = cx
            .debug_bounds("test-checkbox-indicator")
            .expect("checkbox indicator should render");
        let focus = cx
            .debug_bounds("test-checkbox-keyboard-focus")
            .expect("checkbox focus outline should render");

        assert!(focus.left() < indicator.left() && focus.right() > indicator.right());
    }

    #[gpui::test]
    fn pointer_activation_should_not_draw_keyboard_focus_ring(cx: &mut TestAppContext) {
        let (_, _, _, _, _, cx) = toggle_window(cx, false);
        let bounds = cx
            .debug_bounds("test-checkbox")
            .expect("checkbox should render");

        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(cx.debug_bounds("test-checkbox-keyboard-focus"), None);
    }

    struct DirectionRoot;

    impl Render for DirectionRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .child(
                    Switch::new("ltr-switch", "LTR", true)
                        .debug_selector("ltr-switch")
                        .on_change(|_, _, _| {}),
                )
                .child(
                    Switch::new("rtl-switch", "RTL", true)
                        .right_to_left(true)
                        .debug_selector("rtl-switch")
                        .on_change(|_, _, _| {}),
                )
        }
    }

    #[gpui::test]
    fn right_to_left_should_mirror_switch_thumb_position(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        let (_, cx) = cx.add_window_view(|_, _| DirectionRoot);
        cx.run_until_parked();

        let ltr_track = cx.debug_bounds("ltr-switch-indicator").unwrap();
        let ltr_thumb = cx.debug_bounds("ltr-switch-thumb").unwrap();
        let rtl_track = cx.debug_bounds("rtl-switch-indicator").unwrap();
        let rtl_thumb = cx.debug_bounds("rtl-switch-thumb").unwrap();

        assert!(ltr_thumb.center().x > ltr_track.center().x);
        assert!(rtl_thumb.center().x < rtl_track.center().x);
    }

    struct HiddenLabelRoot;

    impl Render for HiddenLabelRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_row()
                .items_start()
                .child(
                    Switch::new("labeled-switch", "Italic text", true)
                        .debug_selector("labeled-switch")
                        .on_change(|_, _, _| {}),
                )
                .child(
                    Switch::new("unlabeled-switch", "Italic text", true)
                        .label_hidden(true)
                        .debug_selector("unlabeled-switch")
                        .on_change(|_, _, _| {}),
                )
        }
    }

    #[gpui::test]
    fn a_hidden_label_should_leave_only_the_indicator_and_still_activate(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        let (_, cx) = cx.add_window_view(|_, _| HiddenLabelRoot);
        cx.run_until_parked();

        let labeled = cx.debug_bounds("labeled-switch").expect("switch renders");
        let unlabeled = cx.debug_bounds("unlabeled-switch").expect("switch renders");
        let indicator = cx
            .debug_bounds("unlabeled-switch-indicator")
            .expect("the indicator remains");

        assert!(unlabeled.size.width < labeled.size.width);
        assert_eq!(unlabeled.size.width, indicator.size.width);
    }

    struct ControlledGeometryRoot {
        checkbox_state: CheckboxState,
        switch_on: bool,
    }

    impl Render for ControlledGeometryRoot {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let checkbox_root = cx.entity().downgrade();
            let switch_root = cx.entity().downgrade();
            div()
                .flex()
                .flex_col()
                .child(
                    Checkbox::new("geometry-checkbox", "Checkbox", self.checkbox_state)
                        .debug_selector("geometry-checkbox")
                        .on_change(move |change, _, cx| {
                            let _ = checkbox_root.update(cx, |root, cx| {
                                root.checkbox_state = change.requested();
                                cx.notify();
                            });
                        }),
                )
                .child(
                    Switch::new("geometry-switch", "Switch", self.switch_on)
                        .debug_selector("geometry-switch")
                        .on_change(move |change, _, cx| {
                            let _ = switch_root.update(cx, |root, cx| {
                                root.switch_on = change.requested();
                                cx.notify();
                            });
                        }),
                )
        }
    }

    fn geometry_window(cx: &mut TestAppContext) -> &mut VisualTestContext {
        cx.set_global(test_theme());
        let (_, cx) = cx.add_window_view(|_, _| ControlledGeometryRoot {
            checkbox_state: CheckboxState::Mixed,
            switch_on: false,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx
    }

    #[gpui::test]
    fn checkbox_activation_should_not_reflow_row_or_indicator(cx: &mut TestAppContext) {
        let cx = geometry_window(cx);
        let row_before = cx.debug_bounds("geometry-checkbox").unwrap();
        let indicator_before = cx.debug_bounds("geometry-checkbox-indicator").unwrap();

        cx.simulate_click(row_before.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(cx.debug_bounds("geometry-checkbox"), Some(row_before));
        assert_eq!(
            cx.debug_bounds("geometry-checkbox-indicator"),
            Some(indicator_before)
        );
    }

    #[gpui::test]
    fn switch_activation_should_not_reflow_row_or_track(cx: &mut TestAppContext) {
        let cx = geometry_window(cx);
        let row_before = cx.debug_bounds("geometry-switch").unwrap();
        let track_before = cx.debug_bounds("geometry-switch-indicator").unwrap();

        cx.simulate_click(row_before.center(), Modifiers::none());
        cx.run_until_parked();

        assert_eq!(cx.debug_bounds("geometry-switch"), Some(row_before));
        assert_eq!(
            cx.debug_bounds("geometry-switch-indicator"),
            Some(track_before)
        );
    }

    #[gpui::test]
    fn switch_thumb_should_be_vertically_centered_in_track(cx: &mut TestAppContext) {
        let cx = geometry_window(cx);
        let track = cx.debug_bounds("geometry-switch-indicator").unwrap();
        let thumb = cx.debug_bounds("geometry-switch-thumb").unwrap();

        assert_eq!(thumb.center().y, track.center().y);
    }
}
