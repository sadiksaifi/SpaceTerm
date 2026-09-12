//! A controlled inline single-selection control over a small set of bounded options.
//!
//! Selection follows keyboard navigation, which is the conventional desktop behavior for a joined
//! segmented control and for a row of selectable option cards: the whole control is one Tab stop,
//! and the arrow keys move the selection directly rather than moving a separate focus cursor that a
//! later key press confirms. Callers own the selected value and receive a change request; the
//! control never retains a value of its own.
//!
//! GPUI 0.2.2 cannot publish radio-group roles or selected state to the native accessibility tree,
//! so each option retains its label as its logical accessibility name without claiming native
//! assistive-technology publication.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, FocusHandle, Global, InteractiveElement as _,
    IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels, RenderOnce, Rgba,
    SharedString, StatefulInteractiveElement as _, StyleRefinement, Styled as _, Window, div,
    prelude::FluentBuilder as _, px, rgba,
};

use crate::{
    ControlShadow,
    tooltip::{Tooltip, TooltipTargetVisibility},
};

/// The greatest number of options one segmented control may present.
///
/// The control is for bounded choices that are all worth showing at once. Longer lists belong in a
/// [`crate::Picker`] or [`crate::ComboBox`], which can scroll and filter.
pub const MAXIMUM_SEGMENTED_OPTIONS: usize = 6;

type SegmentedPreviewBuilder = Rc<dyn Fn(Rgba, Pixels) -> AnyElement>;
type SegmentedChangeHandler<T> = Rc<dyn Fn(&SegmentedChange<T>, &mut Window, &mut App)>;

/// The input path that requested a selection change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentedActivationSource {
    /// A primary pointer press released inside one option.
    Pointer,
    /// An arrow key moved the selection to the nearest enabled option.
    Arrow,
    /// Home or End moved the selection to the first or last enabled option.
    Boundary,
}

/// A controlled selection change request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SegmentedChange<T> {
    previous: Option<T>,
    requested: T,
    source: SegmentedActivationSource,
}

impl<T> SegmentedChange<T> {
    /// Returns the value rendered as current when activation began.
    ///
    /// This is `None` only when the caller's value matched no option, in which case the request is
    /// the user's choice of a value that repairs that state.
    pub fn previous(&self) -> Option<&T> {
        self.previous.as_ref()
    }

    /// Returns the value the user requested.
    pub fn requested(&self) -> &T {
        &self.requested
    }

    /// Returns the input path that requested the change.
    pub fn source(&self) -> SegmentedActivationSource {
        self.source
    }

    /// Consumes the request and returns the requested value.
    pub fn into_requested(self) -> T {
        self.requested
    }
}

/// Standard presentations for the segmented family.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SegmentedSize {
    /// Joined text segments sharing one track, for choices that need only a word each.
    #[default]
    Regular,
    /// Separated option cards that may each carry a preview element above their label.
    Card,
}

/// One option in a segmented control.
pub struct SegmentedOption<T> {
    value: T,
    label: SharedString,
    preview: Option<SegmentedPreviewBuilder>,
    disabled: bool,
    debug_selector: Option<String>,
}

impl<T> SegmentedOption<T> {
    /// Creates an enabled option. Its label is also its logical accessibility name.
    pub fn new(value: T, label: impl Into<SharedString>) -> Self {
        Self {
            value,
            label: label.into(),
            preview: None,
            disabled: false,
            debug_selector: None,
        }
    }

    /// Adds a preview element drawn above the label, built with the resolved label color and the
    /// preview extent. Only [`SegmentedSize::Card`] draws previews.
    pub fn preview(mut self, build: impl Fn(Rgba, Pixels) -> AnyElement + 'static) -> Self {
        self.preview = Some(Rc::new(build));
        self
    }

    /// Controls whether navigation and activation may reach this option.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

/// Paint for one option in one interaction state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedPaint {
    background: Rgba,
    label: Rgba,
    border: Rgba,
}

impl SegmentedPaint {
    /// Creates resolved option paint.
    pub const fn new(background: Rgba, label: Rgba, border: Rgba) -> Self {
        Self {
            background,
            label,
            border,
        }
    }

    /// Returns the option fill.
    pub const fn background(self) -> Rgba {
        self.background
    }

    /// Returns the visible label color.
    pub const fn label(self) -> Rgba {
        self.label
    }

    /// Returns the option border, which also carries the selected ring in card presentation.
    pub const fn border(self) -> Rgba {
        self.border
    }
}

/// Paint for unselected and selected options in one interaction state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedValuePaints {
    unselected: SegmentedPaint,
    selected: SegmentedPaint,
}

impl SegmentedValuePaints {
    /// Creates selection-specific paint.
    pub const fn new(unselected: SegmentedPaint, selected: SegmentedPaint) -> Self {
        Self {
            unselected,
            selected,
        }
    }

    const fn resolve(self, selected: bool) -> SegmentedPaint {
        if selected {
            self.selected
        } else {
            self.unselected
        }
    }
}

/// Complete interaction-state paint for the segmented family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedPaints {
    normal: SegmentedValuePaints,
    hovered: SegmentedValuePaints,
    pressed: SegmentedValuePaints,
    disabled: SegmentedValuePaints,
}

impl SegmentedPaints {
    /// Creates the complete bounded paint catalog.
    pub const fn new(
        normal: SegmentedValuePaints,
        hovered: SegmentedValuePaints,
        pressed: SegmentedValuePaints,
        disabled: SegmentedValuePaints,
    ) -> Self {
        Self {
            normal,
            hovered,
            pressed,
            disabled,
        }
    }
}

/// Geometry and typography for one standard segmented presentation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedMetrics {
    option_height: Pixels,
    preview_height: Pixels,
    minimum_option_width: Pixels,
    option_gap: Pixels,
    horizontal_padding: Pixels,
    radius: Pixels,
    border_width: Pixels,
    focus_gap: Pixels,
    preview_gap: Pixels,
    font_size: Pixels,
    line_height: f32,
}

impl SegmentedMetrics {
    /// Creates desktop geometry for one presentation.
    ///
    /// `preview_height` is used only by [`SegmentedSize::Card`]; pass zero for a text-only
    /// presentation.
    pub fn new(
        option_height: Pixels,
        preview_height: Pixels,
        minimum_option_width: Pixels,
        option_gap: Pixels,
    ) -> Self {
        Self {
            option_height,
            preview_height,
            minimum_option_width,
            option_gap,
            horizontal_padding: px(10.0),
            radius: px(5.0),
            border_width: px(1.0),
            focus_gap: px(2.0),
            preview_gap: px(6.0),
            font_size: px(12.0),
            line_height: 1.2,
        }
    }

    /// Sets the inline padding inside one option.
    pub fn horizontal_padding(mut self, padding: Pixels) -> Self {
        self.horizontal_padding = padding;
        self
    }

    /// Sets option corner rounding.
    pub fn radius(mut self, radius: Pixels) -> Self {
        self.radius = radius;
        self
    }

    /// Sets the stable option border width used in every visual state.
    pub fn border_width(mut self, width: Pixels) -> Self {
        self.border_width = width;
        self
    }

    /// Sets the space between the control and its keyboard focus outline.
    pub fn focus_gap(mut self, gap: Pixels) -> Self {
        self.focus_gap = gap;
        self
    }

    /// Sets the space between a card preview and its label.
    pub fn preview_gap(mut self, gap: Pixels) -> Self {
        self.preview_gap = gap;
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
            option_height: crate::appearance::scale_line_box(
                self.option_height,
                self.font_size,
                text_scale,
                spacing_scale,
            ),
            preview_height: crate::appearance::scale_metric(self.preview_height, spacing_scale),
            minimum_option_width: crate::appearance::scale_metric(
                self.minimum_option_width,
                spacing_scale,
            ),
            option_gap: crate::appearance::scale_metric(self.option_gap, spacing_scale),
            horizontal_padding: crate::appearance::scale_metric(
                self.horizontal_padding,
                spacing_scale,
            ),
            radius: crate::appearance::scale_metric(self.radius, spacing_scale),
            border_width: self.border_width,
            focus_gap: crate::appearance::scale_metric(self.focus_gap, spacing_scale),
            preview_gap: crate::appearance::scale_metric(self.preview_gap, spacing_scale),
            font_size: crate::appearance::scale_metric(self.font_size, text_scale),
            line_height: self.line_height,
        }
    }
}

/// Complete metrics for the segmented family's standard presentations.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedSizes {
    regular: SegmentedMetrics,
    card: SegmentedMetrics,
}

impl SegmentedSizes {
    /// Creates a complete size catalog.
    pub const fn new(regular: SegmentedMetrics, card: SegmentedMetrics) -> Self {
        Self { regular, card }
    }

    const fn resolve(self, size: SegmentedSize) -> SegmentedMetrics {
        match size {
            SegmentedSize::Regular => self.regular,
            SegmentedSize::Card => self.card,
        }
    }

    fn scaled(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            regular: self.regular.scaled(text_scale, spacing_scale),
            card: self.card.scaled(text_scale, spacing_scale),
        }
    }
}

/// Application-owned presentation for segmented controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentedControlTheme {
    paints: SegmentedPaints,
    sizes: SegmentedSizes,
    track_background: Rgba,
    track_border: Rgba,
    focus_border: Rgba,
    selected_shadow: ControlShadow,
}

impl SegmentedControlTheme {
    /// Creates a complete resolved segmented theme.
    pub fn new(
        paints: SegmentedPaints,
        sizes: SegmentedSizes,
        track_background: Rgba,
        track_border: Rgba,
        focus_border: Rgba,
    ) -> Self {
        Self {
            paints,
            sizes,
            track_background,
            track_border,
            focus_border,
            selected_shadow: ControlShadow::none(),
        }
    }

    /// Sets the semantic shadow lifting the selected segment out of its track.
    pub const fn selected_shadow(mut self, shadow: ControlShadow) -> Self {
        self.selected_shadow = shadow;
        self
    }

    /// Returns a copy with text and spacing metrics scaled independently.
    pub fn scaled_metrics(self, text_scale: f32, spacing_scale: f32) -> Self {
        Self {
            sizes: self.sizes.scaled(text_scale, spacing_scale),
            ..self
        }
    }

    fn resolve(self, size: SegmentedSize) -> SegmentedStyle {
        SegmentedStyle {
            paints: self.paints,
            metrics: self.sizes.resolve(size),
            track_background: self.track_background,
            track_border: self.track_border,
            focus_border: self.focus_border,
            selected_shadow: self.selected_shadow,
        }
    }
}

impl Global for SegmentedControlTheme {}

/// A segmented control was built with an unusable option set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentedBuildError {
    /// No option was supplied, so no value could be presented as current.
    EmptyOptions,
    /// More options were supplied than this control family presents at once.
    TooManyOptions,
}

impl std::fmt::Display for SegmentedBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOptions => formatter.write_str("a segmented control requires one option"),
            Self::TooManyOptions => {
                formatter.write_str("a segmented control presents a bounded option set")
            }
        }
    }
}

impl std::error::Error for SegmentedBuildError {}

/// A controlled single-selection control over a bounded option set.
///
/// The first option matching `current` is selected. When none matches, no option is drawn as
/// selected, so malformed caller state never presents two options as current; a change request
/// then carries no previous value.
#[derive(IntoElement)]
pub struct SegmentedControl<T: Clone + PartialEq + 'static> {
    id: ElementId,
    accessibility_name: SharedString,
    options: Vec<SegmentedOption<T>>,
    selected: Option<usize>,
    size: SegmentedSize,
    disabled: bool,
    tab_stop: bool,
    full_width: bool,
    right_to_left: bool,
    debug_selector: Option<String>,
    tooltip: Option<Tooltip>,
    on_change: Option<SegmentedChangeHandler<T>>,
}

impl<T: Clone + PartialEq + 'static> SegmentedControl<T> {
    /// Creates a control presenting at most one option as current.
    ///
    /// # Errors
    ///
    /// Returns [`SegmentedBuildError::EmptyOptions`] when no option is supplied and
    /// [`SegmentedBuildError::TooManyOptions`] beyond [`MAXIMUM_SEGMENTED_OPTIONS`].
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        current: &T,
        options: Vec<SegmentedOption<T>>,
    ) -> Result<Self, SegmentedBuildError> {
        if options.is_empty() {
            return Err(SegmentedBuildError::EmptyOptions);
        }
        if options.len() > MAXIMUM_SEGMENTED_OPTIONS {
            return Err(SegmentedBuildError::TooManyOptions);
        }
        let selected = options.iter().position(|option| &option.value == current);
        Ok(Self {
            id: id.into(),
            accessibility_name: accessibility_name.into(),
            options,
            selected,
            size: SegmentedSize::default(),
            disabled: false,
            tab_stop: true,
            full_width: false,
            right_to_left: false,
            debug_selector: None,
            tooltip: None,
            on_change: None,
        })
    }

    /// Selects a standard presentation.
    pub fn size(mut self, size: SegmentedSize) -> Self {
        self.size = size;
        self
    }

    /// Controls whether the control can request a selection change.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on the control.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.tab_stop = tab_stop;
        self
    }

    /// Distributes the options evenly across the available width.
    pub fn full_width(mut self, full_width: bool) -> Self {
        self.full_width = full_width;
        self
    }

    /// Mirrors option order and arrow-key direction for a right-to-left surrounding layout.
    pub fn right_to_left(mut self, right_to_left: bool) -> Self {
        self.right_to_left = right_to_left;
        self
    }

    /// Adds a stable selector used by GPUI interaction tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Attaches bounded semantic tooltip content to the whole control.
    pub fn tooltip(mut self, tooltip: Tooltip) -> Self {
        self.tooltip = Some(tooltip);
        self
    }

    /// Handles a typed selection change request.
    pub fn on_change(
        mut self,
        handler: impl Fn(&SegmentedChange<T>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }
}

impl<T: Clone + PartialEq + 'static> RenderOnce for SegmentedControl<T> {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = cx
            .try_global::<SegmentedControlTheme>()
            .copied()
            .unwrap_or_else(default_theme)
            .resolve(self.size);
        let metrics = style.metrics;
        let enabled = !self.disabled && self.on_change.is_some();
        let state = window.use_keyed_state(self.id.clone(), cx, |window, cx| {
            SegmentedControlState::new(window, cx)
        });
        let focus_handle = state.read(cx).focus_handle.clone();
        if !enabled && focus_handle.is_focused(window) {
            window.blur();
        }
        state.update(cx, |state, cx| {
            state.synchronize(enabled, self.tab_stop, cx);
        });
        let focused = focus_handle.is_focused(window) && state.read(cx).focus_visible;
        let selector = self
            .debug_selector
            .clone()
            .unwrap_or_else(|| self.accessibility_name.to_string());
        let card = matches!(self.size, SegmentedSize::Card);
        let previous = self
            .selected
            .and_then(|index| self.options.get(index))
            .map(|option| option.value.clone());
        // Navigation outlives the option list. A disabled option is represented by its absence,
        // so keyboard traversal skips exactly what pointer activation refuses.
        let navigable = self
            .options
            .iter()
            .map(|option| (!option.disabled).then(|| option.value.clone()))
            .collect::<Vec<_>>();
        let request = {
            let handler = self.on_change.clone();
            let previous = previous.clone();
            move |requested: T,
                  source: SegmentedActivationSource,
                  window: &mut Window,
                  cx: &mut App| {
                let Some(handler) = &handler else {
                    return;
                };
                if previous.as_ref() == Some(&requested) {
                    return;
                }
                handler(
                    &SegmentedChange {
                        previous: previous.clone(),
                        requested,
                        source,
                    },
                    window,
                    cx,
                );
            }
        };

        let segments = self
            .options
            .iter()
            .enumerate()
            .map(|(index, option)| {
                let selected = self.selected == Some(index);
                let option_enabled = enabled && !option.disabled;
                let paint = if option_enabled {
                    style.paints.normal.resolve(selected)
                } else {
                    style.paints.disabled.resolve(selected)
                };
                let hovered = SegmentedPaintRefinement(style.paints.hovered.resolve(selected));
                let pressed = SegmentedPaintRefinement(style.paints.pressed.resolve(selected));
                let option_selector = option
                    .debug_selector
                    .clone()
                    .unwrap_or_else(|| format!("{selector}-{index}"));
                let pointer_state = state.clone();
                let activate = request.clone();
                let value = option.value.clone();
                div()
                    .id(SharedString::from(format!("{option_selector}-state")))
                    .debug_selector({
                        let option_selector = option_selector.clone();
                        move || option_selector
                    })
                    .relative()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(metrics.preview_gap)
                    .min_w(metrics.minimum_option_width)
                    .min_h(metrics.option_height)
                    .px(metrics.horizontal_padding)
                    .when(self.full_width, |segment| segment.flex_1())
                    .rounded(metrics.radius)
                    .border(metrics.border_width)
                    .border_color(paint.border)
                    .bg(paint.background)
                    .text_color(paint.label)
                    .text_size(metrics.font_size)
                    .line_height(gpui::relative(metrics.line_height))
                    .font(crate::control_typography(cx).regular().clone())
                    .cursor_default()
                    .when(selected && !card, |segment| {
                        segment.shadow(style.selected_shadow.layers())
                    })
                    // Pointer feedback belongs to the segment under the pointer. Reacting to the
                    // track's hover instead would light every segment at once.
                    .when(option_enabled, |segment| {
                        segment
                            .hover(move |style| hovered.segment(style))
                            .active(move |style| pressed.segment(style))
                    })
                    .when(option_enabled, |segment| {
                        segment
                            .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                                pointer_state.update(cx, |state, cx| state.pointer_focus(cx));
                            })
                            .on_click(move |event: &ClickEvent, window, cx| {
                                if !matches!(event, ClickEvent::Mouse(_)) {
                                    return;
                                }
                                window.prevent_default();
                                activate(
                                    value.clone(),
                                    SegmentedActivationSource::Pointer,
                                    window,
                                    cx,
                                );
                                cx.stop_propagation();
                            })
                    })
                    .when_some(
                        option.preview.clone().filter(|_| card),
                        |segment, preview| {
                            segment.child(
                                div()
                                    .flex()
                                    .flex_none()
                                    .w_full()
                                    .h(metrics.preview_height)
                                    .items_center()
                                    .justify_center()
                                    .child(preview(paint.label, metrics.preview_height)),
                            )
                        },
                    )
                    .child(div().flex_none().child(option.label.clone()))
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let keyboard_request = request;
        let key_state = state;
        let selected_index = self.selected;
        let right_to_left = self.right_to_left;
        let track = div()
            .id(self.id)
            .debug_selector({
                let selector = selector.clone();
                move || selector
            })
            .relative()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .gap(metrics.option_gap)
            .when(self.full_width, |track| track.w_full())
            .when(right_to_left, |track| track.flex_row_reverse())
            .when(!card, |track| {
                track
                    .p(metrics.border_width)
                    .rounded(metrics.radius + metrics.border_width)
                    .border(metrics.border_width)
                    .border_color(style.track_border)
                    .bg(style.track_background)
            })
            .cursor_default()
            .block_mouse_except_scroll()
            .when(enabled, |track| {
                track.track_focus(&focus_handle).on_key_down(
                    move |event: &KeyDownEvent, window, cx| {
                        if event.keystroke.modifiers.modified() {
                            return;
                        }
                        let (target, source) = match event.keystroke.key.as_str() {
                            "right" => (
                                step(&navigable, selected_index, !right_to_left),
                                SegmentedActivationSource::Arrow,
                            ),
                            "left" => (
                                step(&navigable, selected_index, right_to_left),
                                SegmentedActivationSource::Arrow,
                            ),
                            "down" => (
                                step(&navigable, selected_index, true),
                                SegmentedActivationSource::Arrow,
                            ),
                            "up" => (
                                step(&navigable, selected_index, false),
                                SegmentedActivationSource::Arrow,
                            ),
                            "home" => (
                                boundary(&navigable, true),
                                SegmentedActivationSource::Boundary,
                            ),
                            "end" => (
                                boundary(&navigable, false),
                                SegmentedActivationSource::Boundary,
                            ),
                            _ => return,
                        };
                        window.prevent_default();
                        cx.stop_propagation();
                        key_state.update(cx, |state, cx| state.keyboard_navigate(cx));
                        if let Some(value) = target.and_then(|index| navigable[index].clone()) {
                            keyboard_request(value, source, window, cx);
                        }
                    },
                )
            })
            .children(segments)
            .when(focused, |track| {
                track.child(focus_outline(
                    metrics,
                    style.focus_border,
                    format!("{selector}-keyboard-focus"),
                ))
            });

        if let Some(tooltip) = self.tooltip {
            tooltip
                .attach(track, TooltipTargetVisibility::Visible)
                .disabled(!enabled)
                .into_any_element()
        } else {
            track.into_any_element()
        }
    }
}

/// Returns the nearest enabled option after `origin` in the requested direction.
///
/// With nothing current, navigation starts at the first enabled option regardless of direction, so
/// a stale caller value is repaired by moving into the set rather than off one of its ends.
fn step<T: Clone>(navigable: &[Option<T>], origin: Option<usize>, forward: bool) -> Option<usize> {
    let Some(origin) = origin else {
        return boundary(navigable, true);
    };
    let mut index = origin;
    loop {
        index = if forward {
            let next = index.checked_add(1)?;
            if next >= navigable.len() {
                return None;
            }
            next
        } else {
            index.checked_sub(1)?
        };
        if navigable[index].is_some() {
            return Some(index);
        }
    }
}

/// Returns the first or last enabled option.
fn boundary<T: Clone>(navigable: &[Option<T>], first: bool) -> Option<usize> {
    if first {
        navigable.iter().position(|value| value.is_some())
    } else {
        navigable.iter().rposition(|value| value.is_some())
    }
}

#[derive(Clone, Copy)]
struct SegmentedStyle {
    paints: SegmentedPaints,
    metrics: SegmentedMetrics,
    track_background: Rgba,
    track_border: Rgba,
    focus_border: Rgba,
    selected_shadow: ControlShadow,
}

#[derive(Clone, Copy)]
struct SegmentedPaintRefinement(SegmentedPaint);

impl SegmentedPaintRefinement {
    fn segment(self, style: StyleRefinement) -> StyleRefinement {
        style
            .bg(self.0.background)
            .border_color(self.0.border)
            .text_color(self.0.label)
    }
}

fn focus_outline(metrics: SegmentedMetrics, color: Rgba, selector: String) -> impl IntoElement {
    let offset = metrics.focus_gap + metrics.border_width;
    div()
        .debug_selector(move || selector)
        .absolute()
        .top(-offset)
        .right(-offset)
        .bottom(-offset)
        .left(-offset)
        .rounded(metrics.radius + metrics.border_width + metrics.focus_gap)
        .border(metrics.border_width)
        .border_color(color)
}

/// Presentation used before the application installs its catalog, so a control rendered during
/// startup is legible rather than invisible.
fn default_theme() -> SegmentedControlTheme {
    let text = rgba(0x1f1f1fff);
    let disabled_text = rgba(0x1f1f1f66);
    let values = SegmentedValuePaints::new(
        SegmentedPaint::new(rgba(0x00000000), text, rgba(0x00000000)),
        SegmentedPaint::new(rgba(0xffffffff), text, rgba(0x00000014)),
    );
    let disabled = SegmentedValuePaints::new(
        SegmentedPaint::new(rgba(0x00000000), disabled_text, rgba(0x00000000)),
        SegmentedPaint::new(rgba(0xffffff80), disabled_text, rgba(0x00000014)),
    );
    SegmentedControlTheme::new(
        SegmentedPaints::new(values, values, values, disabled),
        SegmentedSizes::new(
            SegmentedMetrics::new(px(24.0), px(0.0), px(56.0), px(0.0)),
            SegmentedMetrics::new(px(28.0), px(52.0), px(84.0), px(10.0)),
        ),
        rgba(0x00000010),
        rgba(0x00000014),
        rgba(0x2277ddff),
    )
}

struct SegmentedControlState {
    focus_handle: FocusHandle,
    enabled: bool,
    focus_visible: bool,
}

impl SegmentedControlState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&focus_handle, window, |state, _, cx| {
            state.focus_visible = true;
            cx.notify();
        })
        .detach();
        Self {
            focus_handle,
            enabled: false,
            focus_visible: true,
        }
    }

    fn synchronize(&mut self, enabled: bool, tab_stop: bool, cx: &mut gpui::Context<Self>) {
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && tab_stop);
        if self.enabled != enabled {
            self.enabled = enabled;
            cx.notify();
        }
    }

    fn pointer_focus(&mut self, cx: &mut gpui::Context<Self>) {
        if self.focus_visible {
            self.focus_visible = false;
            cx.notify();
        }
    }

    fn keyboard_navigate(&mut self, cx: &mut gpui::Context<Self>) {
        if !self.focus_visible {
            self.focus_visible = true;
            cx.notify();
        }
    }
}

#[cfg(test)]
#[path = "segmented_control_tests.rs"]
mod tests;
