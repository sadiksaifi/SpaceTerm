use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use gpui::{
    AnyElement, App, BorrowAppContext as _, Bounds, Display, Element, ElementId, Global,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement as _,
    IntoElement, KeyDownEvent, LayoutId, MouseDownEvent, MouseExitEvent, MouseMoveEvent,
    ParentElement as _, Pixels, Point, RenderOnce, Rgba, ScrollWheelEvent, SharedString, Size,
    Style, Styled as _, Task, WeakEntity, Window, WindowId, deferred, div, point,
    prelude::FluentBuilder as _, px,
};

const TOOLTIP_SHOW_DELAY: Duration = Duration::from_millis(500);
const TOOLTIP_OVERLAY_PRIORITY: usize = 0;
const MAX_PRIMARY_CHARACTERS: usize = 512;
const MAX_DETAIL_CHARACTERS: usize = 4096;
const MAX_KEYBOARD_CHARACTERS: usize = 96;

/// Application-owned colors for every tooltip surface.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TooltipPaint {
    background: Rgba,
    border: Rgba,
    primary: Rgba,
    secondary: Rgba,
    keyboard: Rgba,
}

impl TooltipPaint {
    /// Creates the complete bounded tooltip paint catalog.
    pub fn new(
        background: Rgba,
        border: Rgba,
        primary: Rgba,
        secondary: Rgba,
        keyboard: Rgba,
    ) -> Self {
        Self {
            background,
            border,
            primary,
            secondary,
            keyboard,
        }
    }
}

/// Compact desktop metrics shared by every tooltip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TooltipMetrics {
    maximum_width: Pixels,
    horizontal_padding: Pixels,
    vertical_padding: Pixels,
    content_gap: Pixels,
    keyboard_gap: Pixels,
    target_gap: Pixels,
    viewport_margin: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    primary_font_size: Pixels,
    secondary_font_size: Pixels,
    keyboard_font_size: Pixels,
}

impl TooltipMetrics {
    /// Creates compact metrics around the supplied maximum width.
    pub fn new(maximum_width: Pixels) -> Self {
        Self {
            maximum_width: bounded_metric(maximum_width, 160.0, 640.0, 320.0),
            horizontal_padding: px(8.0),
            vertical_padding: px(5.0),
            content_gap: px(3.0),
            keyboard_gap: px(12.0),
            target_gap: px(6.0),
            viewport_margin: px(8.0),
            corner_radius: px(5.0),
            border_width: px(1.0),
            primary_font_size: px(11.0),
            secondary_font_size: px(10.0),
            keyboard_font_size: px(10.0),
        }
    }

    /// Sets compact surface spacing and target placement spacing.
    pub fn spacing(
        mut self,
        horizontal_padding: Pixels,
        vertical_padding: Pixels,
        content_gap: Pixels,
        keyboard_gap: Pixels,
        target_gap: Pixels,
        viewport_margin: Pixels,
    ) -> Self {
        self.horizontal_padding = bounded_metric(horizontal_padding, 0.0, 32.0, 8.0);
        self.vertical_padding = bounded_metric(vertical_padding, 0.0, 24.0, 5.0);
        self.content_gap = bounded_metric(content_gap, 0.0, 16.0, 3.0);
        self.keyboard_gap = bounded_metric(keyboard_gap, 0.0, 32.0, 12.0);
        self.target_gap = bounded_metric(target_gap, 0.0, 24.0, 6.0);
        self.viewport_margin = bounded_metric(viewport_margin, 0.0, 32.0, 8.0);
        self
    }

    /// Sets surface shape metrics.
    pub fn surface(mut self, corner_radius: Pixels, border_width: Pixels) -> Self {
        self.corner_radius = bounded_metric(corner_radius, 0.0, 16.0, 5.0);
        self.border_width = bounded_metric(border_width, 0.0, 4.0, 1.0);
        self
    }

    /// Sets primary, secondary, and keyboard-equivalent font sizes.
    pub fn font_sizes(mut self, primary: Pixels, secondary: Pixels, keyboard: Pixels) -> Self {
        self.primary_font_size = bounded_metric(primary, 8.0, 20.0, 11.0);
        self.secondary_font_size = bounded_metric(secondary, 8.0, 20.0, 10.0);
        self.keyboard_font_size = bounded_metric(keyboard, 8.0, 20.0, 10.0);
        self
    }
}

fn bounded_metric(value: Pixels, minimum: f32, maximum: f32, fallback: f32) -> Pixels {
    let value = f32::from(value);
    px(if value.is_finite() {
        value.clamp(minimum, maximum)
    } else {
        fallback
    })
}

/// Application-installed presentation for every [`Tooltip`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TooltipTheme {
    paint: TooltipPaint,
    metrics: TooltipMetrics,
}

impl TooltipTheme {
    /// Creates a complete tooltip theme from application-owned colors and bounded metrics.
    pub fn new(paint: TooltipPaint, metrics: TooltipMetrics) -> Self {
        Self { paint, metrics }
    }
}

impl Global for TooltipTheme {}

/// Short, semantic contextual help for one noninteractive desktop tooltip.
///
/// A tooltip is delayed, transient, pointer-transparent, and scoped to one Operating-System
/// Window. It never receives focus or pointer input and must not contain actions. `text` is required;
/// optional detail and keyboard-equivalent text remain bounded and are presented through fixed
/// semantic slots rather than arbitrary popup children. A tooltip supplements, but never replaces,
/// the target control's logical accessibility name. Use a Menu or another interactive popover when
/// content must accept focus or input.
#[derive(Clone)]
pub struct Tooltip {
    id: ElementId,
    text: SharedString,
    detail: Option<SharedString>,
    keyboard_equivalent: Option<SharedString>,
    debug_selector: SharedString,
}

impl Tooltip {
    /// Creates a tooltip with stable target identity and required logical text.
    pub fn new(id: impl Into<ElementId>, text: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            text: bounded_text(text.into(), MAX_PRIMARY_CHARACTERS),
            detail: None,
            keyboard_equivalent: None,
            debug_selector: "tooltip".into(),
        }
    }

    /// Adds secondary detail such as a Workspace path.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = nonempty_bounded_text(detail.into(), MAX_DETAIL_CHARACTERS);
        self
    }

    /// Adds a compact keyboard-equivalent label.
    pub fn keyboard_equivalent(mut self, equivalent: impl Into<SharedString>) -> Self {
        self.keyboard_equivalent =
            nonempty_bounded_text(equivalent.into(), MAX_KEYBOARD_CHARACTERS);
        self
    }

    /// Sets the stable selector exposed by the presented tooltip surface.
    pub fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = selector.into();
        self
    }

    /// Attaches this tooltip to an arbitrary GPUI element without changing that element's layout.
    ///
    /// `visibility` must match the wrapped element's effective layout-preserving visibility.
    pub fn attach(
        self,
        target: impl IntoElement,
        visibility: TooltipTargetVisibility,
    ) -> TooltipTarget {
        TooltipTarget {
            tooltip: self,
            target: target.into_any_element(),
            disabled: false,
            visibility,
        }
    }
}

fn bounded_text(text: SharedString, maximum_characters: usize) -> SharedString {
    if text.chars().count() <= maximum_characters {
        return text;
    }
    text.chars()
        .take(maximum_characters)
        .collect::<String>()
        .into()
}

fn nonempty_bounded_text(text: SharedString, maximum_characters: usize) -> Option<SharedString> {
    (!text.is_empty()).then(|| bounded_text(text, maximum_characters))
}

/// The effective layout-preserving visibility of a wrapped tooltip target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TooltipTargetVisibility {
    /// The wrapped target paints and may be hit-tested.
    Visible,
    /// The wrapped target preserves layout but neither paints nor accepts tooltip hit-testing.
    Hidden,
}

impl TooltipTargetVisibility {
    fn is_visible(self) -> bool {
        self == Self::Visible
    }
}

/// A layout-transparent adapter that attaches a [`Tooltip`] to any GPUI element.
///
/// Disabled or hidden targets never schedule or present tooltips. The adapter owns only transient
/// tooltip mechanics; the wrapped element retains its own layout, cursor, focus, accessibility
/// name, and pointer behavior.
#[derive(IntoElement)]
pub struct TooltipTarget {
    tooltip: Tooltip,
    target: AnyElement,
    disabled: bool,
    visibility: TooltipTargetVisibility,
}

impl TooltipTarget {
    /// Controls whether the target may present its tooltip.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

/// A layout-transparent Operating-System Window root for tooltip-wide dismissal behavior.
///
/// Wrap the window's ordinary content once so keyboard input dismisses the active tooltip without
/// consuming that input. Tooltip targets remain independently attachable anywhere below this root.
#[derive(IntoElement)]
pub struct TooltipLayer {
    content: AnyElement,
}

impl TooltipLayer {
    /// Wraps the complete content of one Operating-System Window.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
        }
    }
}

impl RenderOnce for TooltipLayer {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        TooltipLayerElement {
            content: self.content,
        }
    }
}

impl RenderOnce for TooltipTarget {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = window.use_keyed_state(self.tooltip.id.clone(), cx, TooltipTargetState::new);
        let visible = self.visibility.is_visible();
        let enabled = visible && !self.disabled && !self.tooltip.text.is_empty();
        let release = state.update(cx, |state, cx| state.synchronize(enabled, cx));
        if let Some((window_id, reservation)) = release {
            release_window(window_id, reservation, cx);
        }

        let overlay_content = {
            let owner = state.downgrade();
            let state = state.read(cx);
            if state.visible && state.target_bounds.is_some() {
                TooltipOverlay::new(
                    owner,
                    render_surface(
                        &self.tooltip,
                        cx.global::<TooltipTheme>(),
                        window.viewport_size(),
                    ),
                    cx.global::<TooltipTheme>().metrics,
                )
                .into_any_element()
            } else {
                div().into_any_element()
            }
        };
        let overlay = Some(
            deferred(overlay_content)
                .with_priority(TOOLTIP_OVERLAY_PRIORITY)
                .into_any_element(),
        );

        TooltipTargetElement {
            target: self.target,
            overlay,
            state,
            visible,
        }
    }
}

fn render_surface(tooltip: &Tooltip, theme: &TooltipTheme, viewport: Size<Pixels>) -> AnyElement {
    let paint = theme.paint;
    let metrics = theme.metrics;
    let available = available_tooltip_size(viewport, metrics.viewport_margin);
    let keyboard = tooltip.keyboard_equivalent.clone();
    let primary = div()
        .flex()
        .flex_row()
        .items_start()
        .when(keyboard.is_some(), |row| row.justify_between())
        .gap(metrics.keyboard_gap)
        .child(
            div()
                .min_w(px(0.0))
                .flex_grow()
                .whitespace_normal()
                .text_size(metrics.primary_font_size)
                .text_color(paint.primary)
                .child(tooltip.text.clone()),
        )
        .when_some(keyboard, |row, keyboard| {
            row.child(
                div()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_size(metrics.keyboard_font_size)
                    .text_color(paint.keyboard)
                    .child(keyboard),
            )
        });

    div()
        .id(tooltip.debug_selector.clone())
        .debug_selector({
            let selector = tooltip.debug_selector.clone();
            move || selector.to_string()
        })
        .max_w(metrics.maximum_width.min(available.width))
        .max_h(available.height)
        .px(metrics.horizontal_padding)
        .py(metrics.vertical_padding)
        .flex()
        .flex_col()
        .gap(metrics.content_gap)
        .overflow_hidden()
        .rounded(metrics.corner_radius)
        .border(metrics.border_width)
        .border_color(paint.border)
        .bg(paint.background)
        .cursor_default()
        .child(primary)
        .when_some(tooltip.detail.clone(), |surface, detail| {
            surface.child(
                div()
                    .whitespace_normal()
                    .text_size(metrics.secondary_font_size)
                    .text_color(paint.secondary)
                    .child(detail),
            )
        })
        .into_any_element()
}

struct TooltipLayerElement {
    content: AnyElement,
}

impl IntoElement for TooltipLayerElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TooltipLayerElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.content.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.content.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.on_key_event(|_: &KeyDownEvent, phase, window, cx| {
            if phase.capture() {
                dismiss_window_tooltip(window.window_handle().window_id(), cx);
            }
        });
        self.content.paint(window, cx);
    }
}

struct TooltipTargetElement {
    target: AnyElement,
    overlay: Option<AnyElement>,
    state: gpui::Entity<TooltipTargetState>,
    visible: bool,
}

struct TooltipTargetLayout {
    overlay_layout: Option<LayoutId>,
}

struct TooltipTargetPrepaint {
    hitbox: Option<Hitbox>,
}

impl IntoElement for TooltipTargetElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TooltipTargetElement {
    type RequestLayoutState = TooltipTargetLayout;
    type PrepaintState = TooltipTargetPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let target_layout = self.target.request_layout(window, cx);
        let overlay_layout = self
            .overlay
            .as_mut()
            .map(|overlay| overlay.request_layout(window, cx));
        (target_layout, TooltipTargetLayout { overlay_layout })
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let hitbox = self
            .visible
            .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal));
        let state = self.state.clone();
        let target_bounds = self.visible.then_some(bounds);
        let refresh = state.update(cx, |state, cx| state.update_bounds(target_bounds, cx));
        if refresh {
            window.refresh();
        }
        self.target.prepaint(window, cx);
        if let (Some(overlay), Some(layout_id)) =
            (self.overlay.as_mut(), request_layout.overlay_layout)
        {
            window.compute_layout(layout_id, window.viewport_size().into(), cx);
            overlay.prepaint_at(window.layout_bounds(layout_id).origin, window, cx);
        }
        TooltipTargetPrepaint { hitbox }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.target.paint(window, cx);
        if let Some(hitbox) = prepaint.hitbox.clone() {
            register_target_handlers(self.state.clone(), hitbox, window, cx);
        }
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.paint(window, cx);
        }
    }
}

fn register_target_handlers(
    state: gpui::Entity<TooltipTargetState>,
    hitbox: Hitbox,
    window: &mut Window,
    _cx: &mut App,
) {
    let move_state = state.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if !phase.capture() {
            return;
        }
        let hovered = hitbox.is_hovered(window);
        if event.pressed_button.is_some() {
            dismiss_target(&move_state, window, cx);
        } else {
            update_target_hover(&move_state, hovered, window, cx);
        }
    });

    let down_state = state.clone();
    window.on_mouse_event(move |_: &MouseDownEvent, phase, window, cx| {
        if phase.capture() {
            dismiss_target(&down_state, window, cx);
        }
    });

    let scroll_state = state.clone();
    window.on_mouse_event(move |_: &ScrollWheelEvent, phase, window, cx| {
        if phase.capture() {
            dismiss_target(&scroll_state, window, cx);
        }
    });

    let exit_state = state.clone();
    window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
        if phase.capture() {
            update_target_hover(&exit_state, false, window, cx);
        }
    });

    window.on_key_event(move |_: &KeyDownEvent, phase, window, cx| {
        if phase.capture() {
            dismiss_target(&state, window, cx);
        }
    });
}

fn update_target_hover(
    state: &gpui::Entity<TooltipTargetState>,
    hovered: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if !hovered {
        dismiss_target(state, window, cx);
        return;
    }
    if !state.read(cx).can_begin_hover() {
        return;
    }

    let generation = state.update(cx, |state, cx| state.begin_hover(cx));
    let Some(generation) = generation else {
        return;
    };
    let window_id = window.window_handle().window_id();
    let suppression_epoch = tooltip_suppression_epoch(window_id, cx);
    let weak = state.downgrade();
    let task = window.spawn(cx, async move |cx| {
        cx.background_executor().timer(TOOLTIP_SHOW_DELAY).await;
        let _ =
            cx.update(|window, cx| show_target(&weak, generation, suppression_epoch, window, cx));
    });
    state.update(cx, |state, _| state.retain_task(generation, task));
}

fn show_target(
    state: &WeakEntity<TooltipTargetState>,
    generation: u64,
    suppression_epoch: u64,
    window: &mut Window,
    cx: &mut App,
) {
    let window_id = window.window_handle().window_id();
    if tooltip_suppressed(window_id, cx)
        || tooltip_suppression_epoch(window_id, cx) != suppression_epoch
        || !window.is_window_active()
    {
        let _ = state.update(cx, |state, cx| state.cancel_generation(generation, cx));
        return;
    }
    let can_show = state
        .read_with(cx, |state, _| state.can_show(generation))
        .unwrap_or(false);
    if !can_show {
        return;
    }
    let Some(owner) = state.upgrade() else {
        return;
    };
    let (reservation, previous) = reserve_window(&owner, window, cx);
    if let Some(previous) = previous {
        let _ = previous.update(cx, |state, cx| state.dismiss_without_release(cx));
        window.refresh();
    }
    let shown = owner.update(cx, |state, cx| {
        state.show(
            generation,
            reservation,
            window.window_handle().window_id(),
            cx,
        )
    });
    if !shown {
        release_window(window.window_handle().window_id(), reservation, cx);
    }
}

fn dismiss_target(state: &gpui::Entity<TooltipTargetState>, window: &mut Window, cx: &mut App) {
    if !state.read(cx).needs_dismissal() {
        return;
    }

    let release = state.update(cx, |state, cx| state.dismiss(cx));
    if let Some((window_id, reservation)) = release {
        release_window(window_id, reservation, cx);
        window.refresh();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TooltipReservation(u64);

struct TooltipOwnership {
    owner: WeakEntity<TooltipTargetState>,
    reservation: TooltipReservation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum TooltipSuppression {
    Menu,
    CommandPalette,
    Modal,
}

#[derive(Default)]
struct TooltipCoordinator {
    owners: HashMap<WindowId, TooltipOwnership>,
    suppressions: HashMap<WindowId, HashSet<TooltipSuppression>>,
    suppression_epochs: HashMap<WindowId, u64>,
    next_reservation: u64,
}

impl Global for TooltipCoordinator {}

pub(crate) fn init(cx: &mut App) {
    if !cx.has_global::<TooltipCoordinator>() {
        cx.set_global(TooltipCoordinator::default());
    }
}

fn reserve_window(
    owner: &gpui::Entity<TooltipTargetState>,
    window: &Window,
    cx: &mut App,
) -> (TooltipReservation, Option<WeakEntity<TooltipTargetState>>) {
    let window_id = window.window_handle().window_id();
    let weak = owner.downgrade();
    cx.update_global::<TooltipCoordinator, _>(|coordinator, _| {
        coordinator.next_reservation = coordinator.next_reservation.wrapping_add(1);
        let reservation = TooltipReservation(coordinator.next_reservation);
        let previous = coordinator
            .owners
            .insert(
                window_id,
                TooltipOwnership {
                    owner: weak.clone(),
                    reservation,
                },
            )
            .map(|ownership| ownership.owner)
            .filter(|previous| previous != &weak);
        (reservation, previous)
    })
}

fn release_window(window_id: WindowId, reservation: TooltipReservation, cx: &mut App) {
    cx.update_global::<TooltipCoordinator, _>(|coordinator, _| {
        if coordinator
            .owners
            .get(&window_id)
            .is_some_and(|ownership| ownership.reservation == reservation)
        {
            coordinator.owners.remove(&window_id);
        }
        coordinator
            .owners
            .retain(|_, ownership| ownership.owner.upgrade().is_some());
    });
}

fn dismiss_window_tooltip(window_id: WindowId, cx: &mut App) {
    if !cx.has_global::<TooltipCoordinator>() {
        return;
    }
    let displaced = cx.update_global::<TooltipCoordinator, _>(|coordinator, _| {
        coordinator
            .owners
            .remove(&window_id)
            .map(|owner| owner.owner)
    });
    if let Some(displaced) = displaced {
        let _ = displaced.update(cx, |state, cx| state.dismiss_without_release(cx));
    }
}

#[cfg(test)]
pub(crate) fn window_tooltips_suppressed(window: &Window, cx: &App) -> bool {
    tooltip_suppressed(window.window_handle().window_id(), cx)
}

fn tooltip_suppressed(window_id: WindowId, cx: &App) -> bool {
    cx.has_global::<TooltipCoordinator>()
        && cx
            .global::<TooltipCoordinator>()
            .suppressions
            .get(&window_id)
            .is_some_and(|reasons| !reasons.is_empty())
}

fn tooltip_suppression_epoch(window_id: WindowId, cx: &App) -> u64 {
    cx.global::<TooltipCoordinator>()
        .suppression_epochs
        .get(&window_id)
        .copied()
        .unwrap_or_default()
}

pub(crate) fn set_window_tooltip_suppression(
    window_id: WindowId,
    reason: TooltipSuppression,
    suppressed: bool,
    cx: &mut App,
) {
    if !cx.has_global::<TooltipCoordinator>() {
        init(cx);
    }
    let displaced = cx.update_global::<TooltipCoordinator, _>(|coordinator, _| {
        if suppressed {
            let inserted = coordinator
                .suppressions
                .entry(window_id)
                .or_default()
                .insert(reason);
            if inserted {
                let epoch = coordinator.suppression_epochs.entry(window_id).or_default();
                *epoch = epoch.wrapping_add(1);
            }
            coordinator
                .owners
                .remove(&window_id)
                .map(|owner| owner.owner)
        } else {
            if let Some(reasons) = coordinator.suppressions.get_mut(&window_id) {
                reasons.remove(&reason);
                if reasons.is_empty() {
                    coordinator.suppressions.remove(&window_id);
                }
            }
            None
        }
    });
    if let Some(displaced) = displaced {
        let _ = displaced.update(cx, |state, cx| state.dismiss_without_release(cx));
    }
}

struct TooltipTargetState {
    enabled: bool,
    hovered: bool,
    visible: bool,
    generation: u64,
    task: Option<Task<()>>,
    target_bounds: Option<Bounds<Pixels>>,
    ownership: Option<(WindowId, TooltipReservation)>,
}

impl TooltipTargetState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        cx.observe_window_activation(window, |state, window, cx| {
            if !window.is_window_active() {
                let release = state.dismiss(cx);
                if let Some((window_id, reservation)) = release {
                    release_window(window_id, reservation, cx);
                }
            }
        })
        .detach();
        cx.on_release(|state, cx| {
            if let Some((window_id, reservation)) = state.ownership.take() {
                release_window(window_id, reservation, cx);
            }
            state.task.take();
        })
        .detach();
        Self {
            enabled: false,
            hovered: false,
            visible: false,
            generation: 0,
            task: None,
            target_bounds: None,
            ownership: None,
        }
    }

    fn synchronize(
        &mut self,
        enabled: bool,
        cx: &mut gpui::Context<Self>,
    ) -> Option<(WindowId, TooltipReservation)> {
        if self.enabled == enabled {
            return None;
        }
        self.enabled = enabled;
        if enabled { None } else { self.dismiss(cx) }
    }

    fn update_bounds(
        &mut self,
        bounds: Option<Bounds<Pixels>>,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if self.target_bounds == bounds {
            return false;
        }
        self.target_bounds = bounds;
        if self.visible {
            cx.notify();
        }
        self.visible
    }

    fn can_begin_hover(&self) -> bool {
        self.enabled && !self.hovered && !self.visible && self.task.is_none()
    }

    fn begin_hover(&mut self, cx: &mut gpui::Context<Self>) -> Option<u64> {
        if !self.can_begin_hover() {
            return None;
        }
        self.hovered = true;
        self.generation = self.generation.wrapping_add(1);
        cx.notify();
        Some(self.generation)
    }

    fn retain_task(&mut self, generation: u64, task: Task<()>) {
        if self.generation == generation && self.hovered && self.enabled && !self.visible {
            self.task = Some(task);
        }
    }

    fn can_show(&self, generation: u64) -> bool {
        self.enabled
            && self.hovered
            && !self.visible
            && self.generation == generation
            && self.target_bounds.is_some()
    }

    fn cancel_generation(&mut self, generation: u64, cx: &mut gpui::Context<Self>) {
        if self.generation == generation {
            self.task.take();
            self.generation = self.generation.wrapping_add(1);
            cx.notify();
        }
    }

    fn show(
        &mut self,
        generation: u64,
        reservation: TooltipReservation,
        window_id: WindowId,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        if !self.can_show(generation) {
            return false;
        }
        self.task.take();
        self.visible = true;
        self.ownership = Some((window_id, reservation));
        cx.notify();
        true
    }

    fn needs_dismissal(&self) -> bool {
        self.hovered || self.visible || self.task.is_some() || self.ownership.is_some()
    }

    fn dismiss(&mut self, cx: &mut gpui::Context<Self>) -> Option<(WindowId, TooltipReservation)> {
        self.hovered = false;
        self.dismiss_without_release(cx)
    }

    fn dismiss_without_release(
        &mut self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<(WindowId, TooltipReservation)> {
        if !self.visible && self.task.is_none() && self.ownership.is_none() {
            return None;
        }
        self.generation = self.generation.wrapping_add(1);
        self.visible = false;
        self.task.take();
        let ownership = self.ownership.take();
        cx.notify();
        ownership
    }
}

struct TooltipOverlay {
    owner: WeakEntity<TooltipTargetState>,
    child: AnyElement,
    metrics: TooltipMetrics,
}

impl TooltipOverlay {
    fn new(
        owner: WeakEntity<TooltipTargetState>,
        child: AnyElement,
        metrics: TooltipMetrics,
    ) -> Self {
        Self {
            owner,
            child,
            metrics,
        }
    }
}

impl IntoElement for TooltipOverlay {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TooltipOverlay {
    type RequestLayoutState = LayoutId;
    type PrepaintState = bool;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let child_layout = self.child.request_layout(window, cx);
        let available =
            available_tooltip_size(window.viewport_size(), self.metrics.viewport_margin);
        let style = Style {
            display: Display::Flex,
            max_size: gpui::size(available.width.into(), available.height.into()),
            overflow: point(gpui::Overflow::Hidden, gpui::Overflow::Hidden),
            ..Style::default()
        };
        let layout = window.request_layout(style, [child_layout], cx);
        (layout, child_layout)
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let active = self
            .owner
            .read_with(cx, |state, cx| {
                let (window_id, reservation) = state.ownership?;
                let target = state.target_bounds?;
                let authoritative = cx
                    .global::<TooltipCoordinator>()
                    .owners
                    .get(&window_id)
                    .is_some_and(|ownership| ownership.reservation == reservation);
                authoritative.then_some(target)
            })
            .ok()
            .flatten();
        let Some(target) = active else {
            return false;
        };
        let child_bounds = window.layout_bounds(*child_layout);
        let placed = place_tooltip(
            target,
            child_bounds.size,
            window.viewport_size(),
            self.metrics.target_gap,
            self.metrics.viewport_margin,
            window.mouse_position(),
        );
        let offset = placed.origin - child_bounds.origin - bounds.origin;
        window.with_element_offset(offset, |window| self.child.prepaint(window, cx));
        true
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if *prepaint {
            self.child.paint(window, cx);
        }
    }
}

#[derive(Clone, Copy)]
enum TooltipSide {
    Bottom,
    Top,
    Right,
    Left,
}

fn available_tooltip_size(viewport: Size<Pixels>, margin: Pixels) -> Size<Pixels> {
    Size {
        width: (viewport.width - margin * 2.0).max(px(0.0)),
        height: (viewport.height - margin * 2.0).max(px(0.0)),
    }
}

fn place_tooltip(
    target: Bounds<Pixels>,
    panel: Size<Pixels>,
    viewport: Size<Pixels>,
    gap: Pixels,
    margin: Pixels,
    pointer: Point<Pixels>,
) -> Bounds<Pixels> {
    let panel = panel.min(&available_tooltip_size(viewport, margin));
    let sides = [
        TooltipSide::Bottom,
        TooltipSide::Top,
        TooltipSide::Right,
        TooltipSide::Left,
    ];
    let limits = Bounds::new(
        point(margin, margin),
        available_tooltip_size(viewport, margin),
    );
    let candidates = sides.map(|side| {
        let candidate = candidate_bounds(target, panel, gap, side);
        (side, clamp_cross_axis(candidate, limits, side))
    });
    if let Some((_, candidate)) = candidates.iter().copied().find(|(side, candidate)| {
        fits_primary_axis(limits, *candidate, *side) && !candidate.contains(&pointer)
    }) {
        return candidate;
    }
    if let Some((_, candidate)) = candidates
        .iter()
        .copied()
        .find(|(side, candidate)| fits_primary_axis(limits, *candidate, *side))
    {
        return candidate;
    }
    clamp_bounds(candidates[0].1, limits)
}

fn candidate_bounds(
    target: Bounds<Pixels>,
    panel: Size<Pixels>,
    gap: Pixels,
    side: TooltipSide,
) -> Bounds<Pixels> {
    let center_x = target.left() + target.size.width / 2.0;
    let center_y = target.top() + target.size.height / 2.0;
    let origin = match side {
        TooltipSide::Bottom => point(center_x - panel.width / 2.0, target.bottom() + gap),
        TooltipSide::Top => point(
            center_x - panel.width / 2.0,
            target.top() - gap - panel.height,
        ),
        TooltipSide::Right => point(target.right() + gap, center_y - panel.height / 2.0),
        TooltipSide::Left => point(
            target.left() - gap - panel.width,
            center_y - panel.height / 2.0,
        ),
    };
    Bounds::new(origin, panel)
}

#[cfg(test)]
fn contains_bounds(container: Bounds<Pixels>, child: Bounds<Pixels>) -> bool {
    child.left() >= container.left()
        && child.top() >= container.top()
        && child.right() <= container.right()
        && child.bottom() <= container.bottom()
}

fn fits_primary_axis(limits: Bounds<Pixels>, candidate: Bounds<Pixels>, side: TooltipSide) -> bool {
    match side {
        TooltipSide::Bottom | TooltipSide::Top => {
            candidate.top() >= limits.top() && candidate.bottom() <= limits.bottom()
        }
        TooltipSide::Right | TooltipSide::Left => {
            candidate.left() >= limits.left() && candidate.right() <= limits.right()
        }
    }
}

fn clamp_cross_axis(
    mut bounds: Bounds<Pixels>,
    limits: Bounds<Pixels>,
    side: TooltipSide,
) -> Bounds<Pixels> {
    match side {
        TooltipSide::Bottom | TooltipSide::Top => {
            bounds.origin.x = clamp_axis_origin(
                bounds.origin.x,
                bounds.size.width,
                limits.left(),
                limits.right(),
            );
        }
        TooltipSide::Right | TooltipSide::Left => {
            bounds.origin.y = clamp_axis_origin(
                bounds.origin.y,
                bounds.size.height,
                limits.top(),
                limits.bottom(),
            );
        }
    }
    bounds
}

fn clamp_bounds(mut bounds: Bounds<Pixels>, limits: Bounds<Pixels>) -> Bounds<Pixels> {
    bounds.origin.x = clamp_axis_origin(
        bounds.origin.x,
        bounds.size.width,
        limits.left(),
        limits.right(),
    );
    bounds.origin.y = clamp_axis_origin(
        bounds.origin.y,
        bounds.size.height,
        limits.top(),
        limits.bottom(),
    );
    bounds
}

fn clamp_axis_origin(origin: Pixels, length: Pixels, minimum: Pixels, maximum: Pixels) -> Pixels {
    let maximum_origin = (maximum - length).max(minimum);
    px(f32::from(origin).clamp(f32::from(minimum), f32::from(maximum_origin)))
}

#[cfg(test)]
mod tests {
    use gpui::{
        AppContext as _, Context, Entity, FocusHandle, InteractiveElement as _, Modifiers,
        MouseButton, Render, TestAppContext, VisualTestContext, WindowBounds, WindowOptions, rgba,
        size,
    };

    use super::*;

    fn test_theme() -> TooltipTheme {
        TooltipTheme::new(
            TooltipPaint::new(
                rgba(0x202020ff),
                rgba(0x404040ff),
                rgba(0xffffffff),
                rgba(0xaaaaaaff),
                rgba(0xccccccff),
            ),
            TooltipMetrics::new(px(320.0)),
        )
    }

    #[test]
    fn placement_should_prefer_below_the_target() {
        let target = Bounds::new(point(px(100.0), px(100.0)), size(px(40.0), px(20.0)));

        let placed = place_tooltip(
            target,
            size(px(80.0), px(30.0)),
            size(px(400.0), px(300.0)),
            px(6.0),
            px(8.0),
            target.center(),
        );

        assert_eq!(placed.top(), target.bottom() + px(6.0));
    }

    #[test]
    fn placement_should_flip_above_when_below_lacks_space() {
        let target = Bounds::new(point(px(100.0), px(270.0)), size(px(40.0), px(20.0)));

        let placed = place_tooltip(
            target,
            size(px(80.0), px(30.0)),
            size(px(400.0), px(300.0)),
            px(6.0),
            px(8.0),
            target.center(),
        );

        assert_eq!(placed.bottom(), target.top() - px(6.0));
    }

    #[test]
    fn placement_should_clamp_cross_axis_before_flipping_above_a_wide_target() {
        let target = Bounds::new(point(px(0.0), px(220.0)), size(px(240.0), px(20.0)));

        let placed = place_tooltip(
            target,
            size(px(464.0), px(30.0)),
            size(px(480.0), px(260.0)),
            px(6.0),
            px(8.0),
            target.center(),
        );

        assert_eq!(
            placed,
            Bounds::new(point(px(8.0), px(184.0)), size(px(464.0), px(30.0)))
        );
    }

    #[test]
    fn placement_should_constrain_an_oversized_panel_inside_the_viewport_margin() {
        let viewport = size(px(200.0), px(100.0));
        let margin = px(8.0);
        let placed = place_tooltip(
            Bounds::new(point(px(2.0), px(2.0)), size(px(8.0), px(8.0))),
            size(px(500.0), px(500.0)),
            viewport,
            px(6.0),
            margin,
            point(px(4.0), px(4.0)),
        );
        let limits = Bounds::new(
            point(margin, margin),
            available_tooltip_size(viewport, margin),
        );

        assert!(
            contains_bounds(limits, placed),
            "{placed:?} escaped {limits:?}"
        );
    }

    #[test]
    fn content_should_be_bounded_without_splitting_utf8() {
        let tooltip = Tooltip::new("bounded", "é".repeat(MAX_PRIMARY_CHARACTERS + 10));

        assert_eq!(tooltip.text.chars().count(), MAX_PRIMARY_CHARACTERS);
    }

    #[test]
    fn empty_optional_content_should_be_omitted() {
        let tooltip = Tooltip::new("optional", "Primary")
            .detail("")
            .keyboard_equivalent("");

        assert!(tooltip.detail.is_none() && tooltip.keyboard_equivalent.is_none());
    }

    #[test]
    fn theme_metrics_should_clamp_non_finite_and_out_of_range_values() {
        let metrics = TooltipMetrics::new(px(f32::NAN)).font_sizes(px(2.0), px(30.0), px(11.0));

        assert_eq!(metrics.maximum_width, px(320.0));
        assert_eq!(metrics.primary_font_size, px(8.0));
        assert_eq!(metrics.secondary_font_size, px(20.0));
    }

    #[test]
    fn test_theme_should_define_a_noninteractive_surface() {
        let theme = test_theme();

        assert_eq!(theme.metrics.target_gap, px(6.0));
    }

    struct TestRoot {
        show_target: bool,
        disabled: bool,
        target_visibility: TooltipTargetVisibility,
        target_left: Pixels,
        second_target: bool,
        long_detail: bool,
        focus_handle: FocusHandle,
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let detail: SharedString = if self.long_detail {
                "Long ".repeat(MAX_DETAIL_CHARACTERS / "Long ".len()).into()
            } else {
                "Secondary detail".into()
            };
            let target_visibility = self.target_visibility;
            let content = div()
                .relative()
                .size_full()
                .track_focus(&self.focus_handle)
                .when(self.show_target, |root| {
                    root.child(
                        div().absolute().left(self.target_left).top(px(80.0)).child(
                            Tooltip::new("test-tooltip-target", "Primary help")
                                .detail(detail)
                                .keyboard_equivalent("⌘K")
                                .debug_selector("test-tooltip")
                                .attach(
                                    div()
                                        .id("test-tooltip-button")
                                        .debug_selector(|| "test-tooltip-button".to_owned())
                                        .w(px(80.0))
                                        .h(px(24.0))
                                        .bg(rgba(0x404040ff))
                                        .when(
                                            target_visibility == TooltipTargetVisibility::Hidden,
                                            |target| target.invisible(),
                                        ),
                                    target_visibility,
                                )
                                .disabled(self.disabled),
                        ),
                    )
                })
                .when(self.second_target, |root| {
                    root.child(
                        div().absolute().left(px(190.0)).top(px(80.0)).child(
                            Tooltip::new("second-tooltip-target", "Second help")
                                .debug_selector("second-tooltip")
                                .attach(
                                    div()
                                        .id("second-tooltip-button")
                                        .debug_selector(|| "second-tooltip-button".to_owned())
                                        .w(px(80.0))
                                        .h(px(24.0))
                                        .bg(rgba(0x505050ff)),
                                    TooltipTargetVisibility::Visible,
                                ),
                        ),
                    )
                });
            TooltipLayer::new(content)
        }
    }

    fn tooltip_window(cx: &mut TestAppContext) -> (Entity<TestRoot>, &mut VisualTestContext) {
        cx.set_global(test_theme());
        cx.update(init);
        let (root, cx) = cx.add_window_view(|_, cx| TestRoot {
            show_target: true,
            disabled: false,
            target_visibility: TooltipTargetVisibility::Visible,
            target_left: px(80.0),
            second_target: false,
            long_detail: false,
            focus_handle: cx.focus_handle(),
        });
        let focus_handle = root.read_with(cx, |root, _| root.focus_handle.clone());
        cx.update(|window, _| {
            window.activate_window();
            focus_handle.focus(window);
        });
        cx.run_until_parked();
        (root, cx)
    }

    fn target_center(cx: &mut VisualTestContext, selector: &'static str) -> Point<Pixels> {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} was not painted"))
            .center()
    }

    fn hover_for_show_delay(cx: &mut VisualTestContext, selector: &'static str) {
        let center = target_center(cx, selector);
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();
    }

    #[gpui::test]
    fn repeated_non_hover_events_should_not_mutate_or_schedule_an_idle_target(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = tooltip_window(cx);
        let state = cx.update(|window, cx| {
            cx.new(|cx| {
                let mut state = TooltipTargetState::new(window, cx);
                state.synchronize(true, cx);
                state
            })
        });
        let initial_generation = state.read_with(cx, |state, _| state.generation);

        cx.update(|window, cx| {
            for _ in 0..3 {
                update_target_hover(&state, false, window, cx);
            }
        });
        cx.run_until_parked();

        assert_eq!(
            state.read_with(cx, |state, _| (
                state.generation,
                state.hovered,
                state.visible,
                state.task.is_some(),
                state.ownership.is_some(),
            )),
            (initial_generation, false, false, false, false),
        );
    }

    #[gpui::test]
    fn tooltip_should_open_only_after_the_delay(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor()
            .advance_clock(TOOLTIP_SHOW_DELAY - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-tooltip").is_none());

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_some());
    }

    #[gpui::test]
    fn leaving_before_the_delay_should_cancel_opening(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::default());

        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn leaving_after_opening_should_dismiss(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");

        cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::default());
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn pointer_activation_should_dismiss(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");
        let center = target_center(cx, "test-tooltip-button");

        cx.simulate_mouse_down(center, MouseButton::Right, Modifiers::default());
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn keyboard_input_should_dismiss_without_consuming_input(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");

        cx.simulate_keystrokes("a");
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn window_deactivation_should_dismiss(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");

        cx.deactivate_window();
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn removing_the_target_should_cancel_pending_and_visible_state(cx: &mut TestAppContext) {
        let (root, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");

        root.update(cx, |root, cx| {
            root.show_target = false;
            cx.notify();
        });
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn disabled_target_should_never_open(cx: &mut TestAppContext) {
        let (root, cx) = tooltip_window(cx);
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();

        hover_for_show_delay(cx, "test-tooltip-button");

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn layout_preserving_hidden_target_should_cancel_a_pending_tooltip(cx: &mut TestAppContext) {
        let (root, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY / 2);

        root.update(cx, |root, cx| {
            root.target_visibility = TooltipTargetVisibility::Hidden;
            cx.notify();
        });
        cx.run_until_parked();
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn layout_preserving_hidden_target_should_cancel_visible_state_and_not_hit_test(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");
        let center = target_center(cx, "test-tooltip-button");

        root.update(cx, |root, cx| {
            root.target_visibility = TooltipTargetVisibility::Hidden;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::default());
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn tooltip_surface_should_fit_inside_a_minimum_width_viewport(cx: &mut TestAppContext) {
        let mut theme = test_theme();
        theme.metrics = TooltipMetrics::new(px(480.0));
        cx.set_global(theme);
        cx.update(init);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(480.0), px(260.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    cx.new(|cx| TestRoot {
                        show_target: true,
                        disabled: false,
                        target_visibility: TooltipTargetVisibility::Visible,
                        target_left: px(80.0),
                        second_target: false,
                        long_detail: true,
                        focus_handle: cx.focus_handle(),
                    })
                },
            )
            .unwrap_or_else(|error| panic!("tooltip test window failed: {error}"))
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        hover_for_show_delay(&mut cx, "test-tooltip-button");
        let tooltip = cx
            .debug_bounds("test-tooltip")
            .unwrap_or_else(|| panic!("tooltip was not painted"));
        let (viewport, margin) = cx.update(|window, cx| {
            (
                window.viewport_size(),
                cx.global::<TooltipTheme>().metrics.viewport_margin,
            )
        });
        let limits = Bounds::new(
            point(margin, margin),
            available_tooltip_size(viewport, margin),
        );

        assert!(
            contains_bounds(limits, tooltip),
            "{tooltip:?} escaped {limits:?}"
        );
    }

    #[gpui::test]
    fn adjacent_targets_should_transfer_the_single_window_owner(cx: &mut TestAppContext) {
        let (root, cx) = tooltip_window(cx);
        root.update(cx, |root, cx| {
            root.second_target = true;
            cx.notify();
        });
        cx.run_until_parked();
        hover_for_show_delay(cx, "test-tooltip-button");

        hover_for_show_delay(cx, "second-tooltip-button");

        let owner_count = cx.update(|_, cx| cx.global::<TooltipCoordinator>().owners.len());

        assert_eq!(owner_count, 1);
        assert!(cx.debug_bounds("second-tooltip").is_some());
    }

    #[gpui::test]
    fn nested_menu_and_palette_suppression_should_block_window_ownership(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");
        let window_id = cx.update(|window, _| window.window_handle().window_id());

        cx.update(|_, cx| {
            set_window_tooltip_suppression(window_id, TooltipSuppression::Menu, true, cx);
            set_window_tooltip_suppression(window_id, TooltipSuppression::CommandPalette, true, cx);
            set_window_tooltip_suppression(window_id, TooltipSuppression::Menu, false, cx);
        });
        let center = target_center(cx, "test-tooltip-button");
        cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::default());
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();
        let owner_count = cx.update(|_, cx| cx.global::<TooltipCoordinator>().owners.len());

        assert_eq!(owner_count, 0);
    }

    #[gpui::test]
    fn opening_and_closing_suppression_before_the_deadline_should_require_fresh_hover(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");
        let window_id = cx.update(|window, _| window.window_handle().window_id());
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY / 2);

        cx.update(|_, cx| {
            set_window_tooltip_suppression(window_id, TooltipSuppression::Menu, true, cx);
            set_window_tooltip_suppression(window_id, TooltipSuppression::Menu, false, cx);
        });
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-tooltip").is_none());

        cx.simulate_mouse_move(point(px(10.0), px(10.0)), None, Modifiers::default());
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor()
            .advance_clock(TOOLTIP_SHOW_DELAY - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-tooltip").is_none());

        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_some());
    }

    #[gpui::test]
    fn modal_suppression_epoch_should_invalidate_a_delayed_tooltip_after_release(
        cx: &mut TestAppContext,
    ) {
        let (_, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");
        let window_id = cx.update(|window, _| window.window_handle().window_id());
        cx.simulate_mouse_move(center, None, Modifiers::default());
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY / 2);

        cx.update(|_, cx| {
            set_window_tooltip_suppression(window_id, TooltipSuppression::Modal, true, cx);
            set_window_tooltip_suppression(window_id, TooltipSuppression::Modal, false, cx);
        });
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();

        assert!(cx.debug_bounds("test-tooltip").is_none());
    }

    #[gpui::test]
    fn pointer_press_and_drag_should_suppress_pending_tooltip(cx: &mut TestAppContext) {
        let (_, cx) = tooltip_window(cx);
        let center = target_center(cx, "test-tooltip-button");

        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            point(center.x + px(20.0), center.y),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.executor().advance_clock(TOOLTIP_SHOW_DELAY);
        cx.run_until_parked();
        let owner_count = cx.update(|_, cx| cx.global::<TooltipCoordinator>().owners.len());

        assert_eq!(owner_count, 0);
    }

    #[gpui::test]
    fn target_geometry_change_should_reposition_the_visible_tooltip(cx: &mut TestAppContext) {
        let (root, cx) = tooltip_window(cx);
        hover_for_show_delay(cx, "test-tooltip-button");
        let before = cx
            .debug_bounds("test-tooltip")
            .unwrap_or_else(|| panic!("tooltip was not painted"));

        root.update(cx, |root, cx| {
            root.target_left = px(220.0);
            cx.notify();
        });
        cx.run_until_parked();
        let after = cx
            .debug_bounds("test-tooltip")
            .unwrap_or_else(|| panic!("tooltip was not repainted"));

        assert_ne!(before.origin.x, after.origin.x);
    }
}
