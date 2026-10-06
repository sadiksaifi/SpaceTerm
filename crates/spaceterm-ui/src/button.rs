use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

use gpui::{
    AnyElement, App, Bounds, Corners, ElementId, Entity, EntityId, FocusHandle, Global,
    HitboxBehavior, InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, KeyUpEvent,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement,
    Pixels, RenderOnce, Rgba, ScrollAnchor, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, TextRun, WeakFocusHandle, Window, actions,
    canvas, div, prelude::FluentBuilder as _, px,
};

use crate::tooltip::{Tooltip, TooltipTargetVisibility};

const KEY_CONTEXT: &str = "SpaceTermButton";

actions!(spaceterm_button, [CaptureReturn]);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("enter", CaptureReturn, Some(KEY_CONTEXT))]);
}

/// The semantic intent of a button action.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonRole {
    /// An ordinary action.
    #[default]
    Normal,
    /// An action that irreversibly removes or destroys something.
    Destructive,
    /// An action that dismisses the current transient interaction without applying it.
    Cancel,
}

/// The input path that activated a button.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ButtonActivationSource {
    /// A primary pointer press released inside the button.
    Pointer,
    /// An unmodified Space key press while the button had keyboard focus.
    Space,
    /// An unmodified Return or Enter key press while the button had keyboard focus.
    Return,
}

/// Information supplied to a button activation callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ButtonActivation {
    source: ButtonActivationSource,
    role: ButtonRole,
}

impl ButtonActivation {
    /// Returns the input path that activated the button.
    pub fn source(self) -> ButtonActivationSource {
        self.source
    }

    /// Returns the semantic role assigned to the button.
    pub fn role(self) -> ButtonRole {
        self.role
    }
}

/// A bounded visual treatment from the installed button theme.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonVariant {
    /// The highest-emphasis action in the current context.
    Primary,
    /// A neutral filled action.
    #[default]
    Secondary,
    /// A neutral action with a persistent border.
    Outline,
    /// A low-emphasis action that appears primarily on interaction.
    Ghost,
    /// An action that paints no surface in any state, carried by its glyph alone.
    Bare,
    /// An action with destructive consequences.
    Destructive,
    /// A compact text-only command. Navigation remains a separate link control.
    Link,
}

/// Standard control sizes shared by text and icon buttons.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonSize {
    /// Dense controls embedded in compact chrome.
    Compact,
    /// Small controls used by prompts and toolbars.
    #[default]
    Small,
    /// Regular controls used by primary application chrome.
    Regular,
    /// Full-height controls used by prominent rows and footers.
    Large,
}

/// The button's outer silhouette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ButtonShape {
    /// Use the theme's radius for the selected control size.
    #[default]
    Rounded,
    /// Render without rounded corners.
    Square,
    /// Round both ends fully, forming a pill at the selected control height.
    Capsule,
}

/// The edge where a button segment meets its neighbor in one control, such as the two halves of
/// a combo button. The joined corners are square so the segments read as one silhouette.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum JoinedEdge {
    #[default]
    None,
    Leading,
    Trailing,
}

impl JoinedEdge {
    /// The segment's corner radii for the control's outer radius.
    pub(crate) fn corner_radii(self, radius: Pixels) -> Corners<Pixels> {
        let square = px(0.0);
        match self {
            Self::None => Corners::all(radius),
            Self::Leading => Corners {
                top_left: square,
                top_right: radius,
                bottom_right: radius,
                bottom_left: square,
            },
            Self::Trailing => Corners {
                top_left: radius,
                top_right: square,
                bottom_right: square,
                bottom_left: radius,
            },
        }
    }
}

/// Paint values for one visual button state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonPaint {
    background: Rgba,
    foreground: Rgba,
    icon_foreground: Rgba,
    border: Rgba,
    shadow: crate::ControlShadow,
}

impl ButtonPaint {
    /// Creates paint values for a visual button state.
    pub fn new(background: Rgba, foreground: Rgba, border: Rgba) -> Self {
        Self {
            background,
            foreground,
            icon_foreground: foreground,
            border,
            shadow: crate::ControlShadow::none(),
        }
    }

    /// Returns the state's background color.
    pub fn background(self) -> Rgba {
        self.background
    }

    /// Returns the state's foreground color.
    pub fn foreground(self) -> Rgba {
        self.foreground
    }

    /// Sets the foreground for icon-only controls independently of text buttons.
    pub fn icon_foreground(mut self, foreground: Rgba) -> Self {
        self.icon_foreground = foreground;
        self
    }

    /// Returns the state's icon foreground.
    pub fn icon_color(self) -> Rgba {
        self.icon_foreground
    }

    /// Returns the state's border color.
    pub fn border(self) -> Rgba {
        self.border
    }

    /// The paint `level` of the way from this state to `other`.
    ///
    /// A shadow has no partial state, so it switches halfway.
    pub(crate) fn mix(self, other: Self, level: f32) -> Self {
        let mix = |from, to| crate::mix_rgba(from, to, level);
        Self {
            background: mix(self.background, other.background),
            foreground: mix(self.foreground, other.foreground),
            icon_foreground: mix(self.icon_foreground, other.icon_foreground),
            border: mix(self.border, other.border),
            shadow: if level < 0.5 {
                self.shadow
            } else {
                other.shadow
            },
        }
    }
}

/// Paints for every interactive state of one visual variant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonVariantStyle {
    normal: ButtonPaint,
    hovered: ButtonPaint,
    pressed: ButtonPaint,
    disabled: ButtonPaint,
}

impl ButtonVariantStyle {
    /// Creates a variant style from application-owned theme colors.
    pub fn new(
        normal: ButtonPaint,
        hovered: ButtonPaint,
        pressed: ButtonPaint,
        disabled: ButtonPaint,
    ) -> Self {
        Self {
            normal,
            hovered,
            pressed,
            disabled,
        }
    }

    /// Returns the normal-state paint.
    pub fn normal(self) -> ButtonPaint {
        self.normal
    }

    /// Returns the hover-state paint.
    pub fn hovered(self) -> ButtonPaint {
        self.hovered
    }

    /// Returns the pressed-state paint.
    pub fn pressed(self) -> ButtonPaint {
        self.pressed
    }

    /// Returns the disabled-state paint.
    pub fn disabled(self) -> ButtonPaint {
        self.disabled
    }
}

/// Layout metrics for one standard control size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonMetrics {
    height: Pixels,
    icon_button_size: Option<Pixels>,
    icon_baseline_center: Option<Pixels>,
    horizontal_padding: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    font_size: Pixels,
    single_line_height: f32,
    multiline_line_height: f32,
}

impl ButtonMetrics {
    /// Creates metrics for a control height with compact native defaults.
    pub fn new(height: Pixels) -> Self {
        Self {
            height,
            icon_button_size: None,
            icon_baseline_center: None,
            horizontal_padding: px(8.0),
            gap: px(6.0),
            corner_radius: px(5.0),
            border_width: px(1.0),
            font_size: px(12.0),
            single_line_height: 1.0,
            multiline_line_height: 1.2,
        }
    }

    /// Overrides the square pointer target used only by icon buttons.
    ///
    /// Text buttons keep `height`; this lets an application apply its icon hit-target policy
    /// without changing label geometry. The resolved target is already density-aware, so metric
    /// scaling leaves an explicit override unchanged.
    pub fn icon_button_size(mut self, size: Pixels) -> Self {
        self.icon_button_size = Some(size.max(px(0.0)));
        self
    }

    /// Sets the center-above-baseline metric for icons paired with this size's text label.
    pub fn icon_baseline_center(mut self, center: Pixels) -> Self {
        self.icon_baseline_center = Some(center);
        self
    }

    /// Sets horizontal padding for text buttons.
    pub fn horizontal_padding(mut self, padding: Pixels) -> Self {
        self.horizontal_padding = padding;
        self
    }

    /// Sets spacing between a text button's label and decorations.
    pub fn gap(mut self, gap: Pixels) -> Self {
        self.gap = gap;
        self
    }

    /// Sets the rounded shape's corner radius.
    pub fn corner_radius(mut self, radius: Pixels) -> Self {
        self.corner_radius = radius;
        self
    }

    /// Sets the stable border width used in every visual state.
    pub fn border_width(mut self, width: Pixels) -> Self {
        self.border_width = width;
        self
    }

    /// Sets the text label size.
    pub fn font_size(mut self, size: Pixels) -> Self {
        self.font_size = size;
        self
    }

    /// Sets relative line heights for single-line and multiline labels.
    pub fn line_heights(mut self, single_line: f32, multiline: f32) -> Self {
        self.single_line_height = single_line.clamp(1.0, 2.0);
        self.multiline_line_height = multiline.clamp(1.0, 2.0);
        self
    }

    fn scaled(self, spacing_scale: f32) -> Self {
        Self {
            height: crate::appearance::scale_line_box(self.height, self.font_size, spacing_scale),
            horizontal_padding: crate::appearance::scale_metric(
                self.horizontal_padding,
                spacing_scale,
            ),
            gap: crate::appearance::scale_metric(self.gap, spacing_scale),
            ..self
        }
    }
}

/// The complete set of visual variants required by the button API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonVariants {
    primary: ButtonVariantStyle,
    secondary: ButtonVariantStyle,
    outline: ButtonVariantStyle,
    ghost: ButtonVariantStyle,
    bare: ButtonVariantStyle,
    destructive: ButtonVariantStyle,
    link: ButtonVariantStyle,
}

impl ButtonVariants {
    /// Creates a complete bounded variant catalog.
    pub fn new(
        primary: ButtonVariantStyle,
        secondary: ButtonVariantStyle,
        outline: ButtonVariantStyle,
        ghost: ButtonVariantStyle,
        bare: ButtonVariantStyle,
        destructive: ButtonVariantStyle,
        link: ButtonVariantStyle,
    ) -> Self {
        Self {
            primary,
            secondary,
            outline,
            ghost,
            bare,
            destructive,
            link,
        }
    }

    fn resolve(self, variant: ButtonVariant) -> ButtonVariantStyle {
        match variant {
            ButtonVariant::Primary => self.primary,
            ButtonVariant::Secondary => self.secondary,
            ButtonVariant::Outline => self.outline,
            ButtonVariant::Ghost => self.ghost,
            ButtonVariant::Bare => self.bare,
            ButtonVariant::Destructive => self.destructive,
            ButtonVariant::Link => self.link,
        }
    }
}

/// The complete set of standard button metrics.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonSizes {
    compact: ButtonMetrics,
    small: ButtonMetrics,
    regular: ButtonMetrics,
    large: ButtonMetrics,
}

impl ButtonSizes {
    /// Creates the standard size catalog.
    pub fn new(
        compact: ButtonMetrics,
        small: ButtonMetrics,
        regular: ButtonMetrics,
        large: ButtonMetrics,
    ) -> Self {
        Self {
            compact,
            small,
            regular,
            large,
        }
    }

    fn resolve(self, size: ButtonSize) -> ButtonMetrics {
        match size {
            ButtonSize::Compact => self.compact,
            ButtonSize::Small => self.small,
            ButtonSize::Regular => self.regular,
            ButtonSize::Large => self.large,
        }
    }

    fn scaled(self, spacing_scale: f32) -> Self {
        Self {
            compact: self.compact.scaled(spacing_scale),
            small: self.small.scaled(spacing_scale),
            regular: self.regular.scaled(spacing_scale),
            large: self.large.scaled(spacing_scale),
        }
    }
}

/// Application-owned presentation installed once for every reusable button.
///
/// The component owns interaction semantics and a bounded visual vocabulary while the application
/// supplies product colors and native control metrics from its canonical theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonTheme {
    variants: ButtonVariants,
    sizes: ButtonSizes,
    focus_border: Rgba,
}

impl ButtonTheme {
    /// Creates a complete button theme.
    pub fn new(variants: ButtonVariants, sizes: ButtonSizes, focus_border: Rgba) -> Self {
        Self {
            variants,
            sizes,
            focus_border,
        }
    }

    /// Returns the resolved state paints for a semantic variant.
    pub fn paints(self, variant: ButtonVariant) -> ButtonVariantStyle {
        self.variants.resolve(variant)
    }

    /// Adds the application's raised treatment to ordinary buttons. Pressed and disabled
    /// controls do not cast the resting shadow.
    pub fn secondary_elevation(
        mut self,
        shadow: crate::ControlShadow,
        border: Option<Rgba>,
    ) -> Self {
        let ordinary = &mut self.variants.secondary;
        for paint in [&mut ordinary.normal, &mut ordinary.hovered] {
            paint.shadow = shadow;
            if let Some(border) = border {
                paint.border = border;
            }
        }
        ordinary.pressed.shadow = crate::ControlShadow::none();
        if let Some(border) = border {
            ordinary.pressed.border = border;
        }
        ordinary.disabled.shadow = crate::ControlShadow::none();
        self
    }

    /// Sets ordinary-control borders without changing fills, shadows, or focus indicators.
    pub fn secondary_borders(mut self, borders: crate::ControlBorderStates) -> Self {
        let ordinary = &mut self.variants.secondary;
        ordinary.normal.border = borders.normal;
        ordinary.hovered.border = borders.hovered;
        ordinary.pressed.border = borders.pressed;
        ordinary.disabled.border = borders.disabled;
        self
    }

    /// Returns the outer side length of an icon button in this theme.
    pub fn icon_button_size(self, size: ButtonSize) -> Pixels {
        let metrics = self.sizes.resolve(size);
        metrics.icon_button_size.unwrap_or(metrics.height)
    }

    /// Returns the outer height of a text button in this theme.
    pub fn control_height(self, size: ButtonSize) -> Pixels {
        self.sizes.resolve(size).height
    }

    /// Returns the keyboard focus-ring paint.
    #[cfg(test)]
    pub(crate) fn focus_border(self) -> Rgba {
        self.focus_border
    }

    pub(crate) fn scaled_spacing(self, spacing_scale: f32) -> Self {
        Self {
            sizes: self.sizes.scaled(spacing_scale),
            ..self
        }
    }

    fn resolve(self, variant: ButtonVariant, size: ButtonSize, shape: ButtonShape) -> ButtonStyle {
        let variant = self.variants.resolve(variant);
        ButtonStyle::new(variant, self.focus_border, self.sizes.resolve(size), shape)
    }
}

impl Global for ButtonTheme {}

/// Resolves a button treatment from the theme of the surface the control rests on, for another
/// control that paints itself as part of a button, such as a combo button's menu segment.
pub(crate) fn resolve_button_style(
    variant: ButtonVariant,
    size: ButtonSize,
    shape: ButtonShape,
    cx: &App,
) -> ButtonStyle {
    crate::floating_surface::hosted_button_theme(cx).resolve(variant, size, shape)
}

pub(crate) fn measure_button_intrinsic_width(
    theme: &ButtonTheme,
    label: &SharedString,
    size: ButtonSize,
    window: &Window,
    cx: &App,
) -> Pixels {
    let style = theme.resolve(ButtonVariant::Secondary, size, ButtonShape::Rounded);
    let text_style = window.text_style();
    let font = crate::control_typography(cx).regular().clone();
    let run = TextRun {
        len: label.len(),
        font,
        color: text_style.color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    window
        .text_system()
        .shape_line(label.clone(), style.font_size, &[run], None)
        .width
        + style.horizontal_padding * 2.0
        + style.border_width * 2.0
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ButtonStyle {
    pub(crate) normal: ButtonPaint,
    pub(crate) hovered: ButtonPaint,
    pub(crate) pressed: ButtonPaint,
    pub(crate) disabled: ButtonPaint,
    pub(crate) focus_border: Rgba,
    pub(crate) height: Pixels,
    icon_button_size: Pixels,
    icon_baseline_center: Option<Pixels>,
    pub(crate) horizontal_padding: Pixels,
    gap: Pixels,
    pub(crate) corner_radius: Pixels,
    pub(crate) border_width: Pixels,
    font_size: Pixels,
    single_line_height: f32,
    multiline_line_height: f32,
}

impl ButtonStyle {
    fn new(
        paints: ButtonVariantStyle,
        focus_border: Rgba,
        metrics: ButtonMetrics,
        shape: ButtonShape,
    ) -> Self {
        Self {
            normal: paints.normal,
            hovered: paints.hovered,
            pressed: paints.pressed,
            disabled: paints.disabled,
            focus_border,
            height: metrics.height,
            icon_button_size: metrics.icon_button_size.unwrap_or(metrics.height),
            icon_baseline_center: metrics.icon_baseline_center,
            horizontal_padding: metrics.horizontal_padding,
            gap: metrics.gap,
            corner_radius: match shape {
                ButtonShape::Rounded => metrics.corner_radius,
                ButtonShape::Square => px(0.0),
                ButtonShape::Capsule => metrics.height / 2.0,
            },
            border_width: metrics.border_width,
            font_size: metrics.font_size,
            single_line_height: metrics.single_line_height,
            multiline_line_height: metrics.multiline_line_height,
        }
    }

    fn paints(self) -> ButtonVariantStyle {
        ButtonVariantStyle::new(self.normal, self.hovered, self.pressed, self.disabled)
    }
}

type ActivationHandler = Rc<dyn Fn(&ButtonActivation, &mut Window, &mut App)>;
type ContentBuilder = Box<dyn FnOnce(Rgba) -> AnyElement>;
type ModalPressCancellation = Rc<dyn Fn(&mut App)>;
#[cfg(test)]
type ModalPressIdleCheck = Rc<dyn Fn(&App) -> bool>;
type ModalPressLivenessCheck = Rc<dyn Fn() -> bool>;

struct ModalPressRegistration {
    cancel: ModalPressCancellation,
    #[cfg(test)]
    is_idle: ModalPressIdleCheck,
    is_alive: ModalPressLivenessCheck,
}

#[derive(Clone, Default)]
pub(crate) struct ModalPressOwner {
    controls: Rc<RefCell<HashMap<EntityId, ModalPressRegistration>>>,
}

impl ModalPressOwner {
    pub(crate) fn register<T: 'static>(
        &self,
        state: &Entity<T>,
        cancel: impl Fn(&mut T, &mut gpui::Context<T>) + 'static,
        _is_idle: impl Fn(&T) -> bool + 'static,
    ) {
        let cancel_state = state.downgrade();
        #[cfg(test)]
        let idle_state = state.downgrade();
        let live_state = state.downgrade();
        let registration = ModalPressRegistration {
            cancel: Rc::new(move |cx| {
                let _ = cancel_state.update(cx, |state, cx| cancel(state, cx));
            }),
            #[cfg(test)]
            is_idle: Rc::new(move |cx| {
                idle_state
                    .read_with(cx, |state, _| _is_idle(state))
                    .unwrap_or(true)
            }),
            is_alive: Rc::new(move || live_state.upgrade().is_some()),
        };
        let mut controls = self.controls.borrow_mut();
        controls.retain(|_, control| (control.is_alive)());
        controls.insert(state.entity_id(), registration);
    }

    pub(crate) fn disarm(&self, cx: &mut App) {
        let controls = self
            .controls
            .borrow()
            .values()
            .map(|control| control.cancel.clone())
            .collect::<Vec<_>>();
        for cancel in controls {
            cancel(cx);
        }
        self.controls
            .borrow_mut()
            .retain(|_, control| (control.is_alive)());
    }

    #[cfg(test)]
    pub(crate) fn controls_are_idle(&self, cx: &App) -> bool {
        self.controls
            .borrow()
            .values()
            .all(|control| (control.is_idle)(cx))
    }
}

#[derive(Clone)]
pub(crate) struct ModalFocusAnchorRegistry {
    scroll_handle: ScrollHandle,
    frame: Rc<Cell<u64>>,
    registrations: Rc<RefCell<Vec<ModalFocusAnchorRegistration>>>,
}

struct ModalFocusAnchorRegistration {
    focus: WeakFocusHandle,
    control: ModalFocusAnchor,
    frame: u64,
}

#[derive(Clone, Copy)]
struct TrackedFocusBounds {
    bounds: Bounds<Pixels>,
    scroll_offset: gpui::Point<Pixels>,
}

#[derive(Clone)]
pub(crate) struct ModalFocusAnchor {
    anchor: ScrollAnchor,
    scroll_handle: ScrollHandle,
    bounds: Rc<Cell<Option<TrackedFocusBounds>>>,
}

impl ModalFocusAnchor {
    pub(crate) fn scroll_anchor(&self) -> ScrollAnchor {
        self.anchor.clone()
    }

    pub(crate) fn track_bounds(&self, bounds: Bounds<Pixels>) {
        self.bounds.set(Some(TrackedFocusBounds {
            bounds,
            scroll_offset: self.scroll_handle.offset(),
        }));
    }

    pub(crate) fn bounds_tracker(&self, inset: Pixels) -> AnyElement {
        let bounds = self.bounds.clone();
        let scroll_handle = self.scroll_handle.clone();
        canvas(
            move |control_bounds, _, _| {
                bounds.set(Some(TrackedFocusBounds {
                    bounds: control_bounds.dilate(inset),
                    scroll_offset: scroll_handle.offset(),
                }));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0()
        .into_any_element()
    }
}

impl ModalFocusAnchorRegistry {
    pub(crate) fn new(scroll_handle: ScrollHandle) -> Self {
        Self {
            scroll_handle,
            frame: Rc::new(Cell::new(0)),
            registrations: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub(crate) fn reset(&self) {
        let previous = self.frame.get();
        self.frame.set(previous.wrapping_add(1));
        self.registrations.borrow_mut().retain(|registration| {
            registration.frame == previous && registration.focus.upgrade().is_some()
        });
    }

    pub(crate) fn register(&self, focus: &FocusHandle) -> ModalFocusAnchor {
        let frame = self.frame.get();
        let mut registrations = self.registrations.borrow_mut();
        registrations.retain(|registration| registration.focus.upgrade().is_some());
        if let Some(registration) = registrations
            .iter_mut()
            .find(|registration| registration.focus.eq(focus))
        {
            registration.frame = frame;
            return registration.control.clone();
        }
        let control = ModalFocusAnchor {
            anchor: ScrollAnchor::for_handle(self.scroll_handle.clone()),
            scroll_handle: self.scroll_handle.clone(),
            bounds: Rc::new(Cell::new(None)),
        };
        registrations.push(ModalFocusAnchorRegistration {
            focus: focus.downgrade(),
            control: control.clone(),
            frame,
        });
        control
    }

    pub(crate) fn reveal(&self, focus: &FocusHandle, window: &mut Window, cx: &mut App) -> bool {
        let frame = self.frame.get();
        let control = {
            let mut registrations = self.registrations.borrow_mut();
            registrations.retain(|registration| registration.focus.upgrade().is_some());
            registrations
                .iter()
                .find(|registration| registration.frame == frame && registration.focus.eq(focus))
                .map(|registration| registration.control.clone())
        };
        let Some(control) = control else {
            return false;
        };
        if let Some(tracked) = control.bounds.get() {
            let mut bounds = tracked.bounds;
            let viewport = self.scroll_handle.bounds();
            let previous_offset = self.scroll_handle.offset();
            bounds.origin.x += previous_offset.x - tracked.scroll_offset.x;
            bounds.origin.y += previous_offset.y - tracked.scroll_offset.y;
            let mut offset = previous_offset;
            if bounds.top() < viewport.top() {
                offset.y += viewport.top() - bounds.top();
            } else if bounds.bottom() > viewport.bottom() {
                offset.y += viewport.bottom() - bounds.bottom();
            }
            if offset != previous_offset {
                self.scroll_handle.set_offset(offset);
                window.refresh();
            }
        } else {
            control.anchor.scroll_to(window, cx);
        }
        true
    }
}

#[derive(Clone)]
pub(crate) struct ModalControlScope {
    press_owner: ModalPressOwner,
    focus_anchors: Option<ModalFocusAnchorRegistry>,
}

thread_local! {
    static CURRENT_MODAL_CONTROL_SCOPE: RefCell<Option<ModalControlScope>> = const {
        RefCell::new(None)
    };
}

struct ModalControlScopeGuard {
    previous: Option<ModalControlScope>,
}

impl Drop for ModalControlScopeGuard {
    fn drop(&mut self) {
        CURRENT_MODAL_CONTROL_SCOPE.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

impl ModalControlScope {
    pub(crate) fn new(press_owner: ModalPressOwner) -> Self {
        Self {
            press_owner,
            focus_anchors: None,
        }
    }

    pub(crate) fn with_focus_anchors(mut self, focus_anchors: ModalFocusAnchorRegistry) -> Self {
        self.focus_anchors = Some(focus_anchors);
        self
    }

    pub(crate) fn enter<R>(&self, render: impl FnOnce() -> R) -> R {
        let previous =
            CURRENT_MODAL_CONTROL_SCOPE.with(|current| current.replace(Some(self.clone())));
        let _guard = ModalControlScopeGuard { previous };
        render()
    }

    fn current_press_owner() -> Option<ModalPressOwner> {
        CURRENT_MODAL_CONTROL_SCOPE.with(|current| {
            current
                .borrow()
                .as_ref()
                .map(|scope| scope.press_owner.clone())
        })
    }

    pub(crate) fn register_current_focus_anchor(focus: &FocusHandle) -> Option<ModalFocusAnchor> {
        CURRENT_MODAL_CONTROL_SCOPE.with(|current| {
            current
                .borrow()
                .as_ref()
                .and_then(|scope| scope.focus_anchors.as_ref())
                .map(|anchors| anchors.register(focus))
        })
    }
}

/// A reusable text action button with native desktop press semantics.
#[derive(IntoElement)]
pub struct Button {
    core: ButtonCore,
    label: SharedString,
    leading: Option<ContentBuilder>,
    trailing: Option<ContentBuilder>,
    shortcut: Option<SharedString>,
    full_width: bool,
    align_start: bool,
    multiline: bool,
}

impl Button {
    /// Pins only presentation for the development acceptance gallery.
    #[cfg(feature = "control-preview")]
    pub fn preview_state(mut self, state: crate::ControlPreviewState) -> Self {
        self.core.preview_state = Some(state);
        self
    }

    /// Supplies complete paints resolved for a contextual meaning or host, such as a warning.
    /// Interaction, disabled state, focus geometry, and metrics remain owned by the button.
    pub fn contextual_style(mut self, style: ButtonVariantStyle, focus_border: Rgba) -> Self {
        self.core.contextual_style = Some((style, focus_border));
        self
    }

    /// Supplies complete metrics resolved for a host's own geometry and density, such as a row
    /// in a sidebar. The metrics replace the standard size; paints, interaction, and focus remain
    /// owned by the button.
    pub fn contextual_metrics(mut self, metrics: ButtonMetrics) -> Self {
        self.core.contextual_metrics = Some(metrics);
        self
    }

    /// Creates a small secondary text button. Its label is also its logical accessibility name.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            core: ButtonCore::new(id.into(), label.clone()),
            label,
            leading: None,
            trailing: None,
            shortcut: None,
            full_width: false,
            align_start: false,
            multiline: false,
        }
    }

    pub(crate) fn modal_focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.core.injected_focus_handle = Some(focus_handle);
        self.core.tab_stop = true;
        self
    }

    /// Tracks keyboard focus with `focus_handle`, which the button's owner retains across frames,
    /// instead of a handle the button creates.
    pub(crate) fn focus_handle(mut self, focus_handle: FocusHandle) -> Self {
        self.core.injected_focus_handle = Some(focus_handle);
        self
    }

    /// Leaves the focus ring to a composing control, which paints it after its other parts.
    pub(crate) fn parent_draws_focus_ring(mut self) -> Self {
        self.core.parent_draws_focus_ring = true;
        self
    }

    pub(crate) fn modal_borderless(mut self) -> Self {
        self.core.modal_borderless = true;
        self
    }

    pub(crate) fn modal_press_owner(mut self, owner: ModalPressOwner) -> Self {
        self.core.modal_press_owner = Some(owner);
        self
    }

    pub(crate) fn multiline(mut self, multiline: bool) -> Self {
        self.multiline = multiline;
        self
    }

    /// Squares the corners on `edge`, where a neighboring segment of the same control joins.
    pub(crate) fn joined_edge(mut self, edge: JoinedEdge) -> Self {
        self.core.joined_edge = edge;
        self
    }

    /// Insets the content from each edge by the label gap, so the edges, the label, and the
    /// Shortcut are evenly spaced.
    pub(crate) fn even_spacing(mut self) -> Self {
        self.core.even_spacing = true;
        self
    }

    /// Shows the displayed Shortcut that activates the button after its label, in the shortcut
    /// font. The owner binds the keystroke; the Shortcut stays out of the accessibility name.
    pub fn shortcut(mut self, shortcut: impl Into<SharedString>) -> Self {
        self.shortcut = Some(shortcut.into());
        self
    }

    /// Adds leading noninteractive content rendered with the resolved foreground color.
    pub fn leading(mut self, build: impl FnOnce(Rgba) -> AnyElement + 'static) -> Self {
        self.leading = Some(Box::new(build));
        self
    }

    /// Adds trailing noninteractive content rendered with the resolved foreground color.
    pub fn trailing(mut self, build: impl FnOnce(Rgba) -> AnyElement + 'static) -> Self {
        self.trailing = Some(Box::new(build));
        self
    }

    /// Makes the button fill the available width.
    pub fn full_width(mut self, full_width: bool) -> Self {
        self.full_width = full_width;
        self
    }

    /// Leads a full-width button's content from its start edge, the way a list row reads,
    /// instead of centering it.
    pub fn align_start(mut self) -> Self {
        self.align_start = true;
        self
    }

    /// Selects a bounded visual treatment from the installed button theme.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.core.variant = variant;
        self
    }

    /// Selects a standard native control size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.core.size = size;
        self
    }

    /// Selects the outer silhouette independently from visual emphasis.
    pub fn shape(mut self, shape: ButtonShape) -> Self {
        self.core.shape = shape;
        self
    }

    /// Assigns the semantic intent of the action.
    pub fn role(mut self, role: ButtonRole) -> Self {
        self.core.role = role;
        self
    }

    /// Controls whether the button can activate.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.core.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on this button.
    ///
    /// This defaults to `false` so compact terminal chrome does not capture Tab. A containing form
    /// or dialog may opt in and route traversal according to its focus policy.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.core.tab_stop = tab_stop;
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

    /// Handles successful pointer or keyboard activation.
    pub fn on_activate(
        mut self,
        handler: impl Fn(&ButtonActivation, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.core.on_activate = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.core.resolve_style(cx);
        let full_width = self.full_width;
        let align_start = self.align_start;
        let multiline = self.multiline;
        let has_leading = self.leading.is_some();
        let has_trailing = self.trailing.is_some();
        let even_spacing = self.core.even_spacing;
        let shortcut_font = crate::control_typography(cx).shortcut().clone();
        let selector = match &self.core.debug_selector {
            Some(selector) => selector.clone(),
            None => self.label.to_string(),
        };
        let label_selector = format!("{selector}-label");
        let shortcut_selector = format!("{selector}-shortcut");
        let icon_offset = if has_leading || has_trailing {
            style.icon_baseline_center.map(|center| {
                crate::icon::text_alignment_offset(
                    crate::control_typography(cx).regular(),
                    style.font_size,
                    style.font_size * style.single_line_height,
                    center,
                    window,
                )
            })
        } else {
            None
        };
        let content = move |_foreground, icon_foreground| {
            div()
                .flex()
                .min_w_0()
                .items_center()
                .map(|content| {
                    if align_start {
                        content.justify_start()
                    } else {
                        content.justify_center()
                    }
                })
                .gap(style.gap)
                .when(full_width, |content| content.w_full())
                .when_some(self.leading, |content, build| {
                    content.child(
                        div()
                            .relative()
                            .when_some(icon_offset, |icon, offset| icon.top(offset))
                            .child(build(icon_foreground)),
                    )
                })
                .child(
                    div()
                        .debug_selector(move || label_selector)
                        .min_w_0()
                        .line_height(gpui::relative(if multiline {
                            style.multiline_line_height
                        } else {
                            style.single_line_height
                        }))
                        .when(multiline, |label| label.whitespace_normal().text_center())
                        .map(|label| {
                            // Evenly spaced content measures its spacing to the label's ink.
                            if even_spacing && !multiline {
                                label.child(crate::optical_text::OpticalText::new(self.label))
                            } else {
                                label.child(self.label)
                            }
                        }),
                )
                .when_some(self.shortcut, |content, shortcut| {
                    content.child(
                        div()
                            .debug_selector(move || shortcut_selector)
                            .flex_none()
                            .font(shortcut_font)
                            .line_height(gpui::relative(style.single_line_height))
                            .child(crate::ShortcutLabel::new(shortcut)),
                    )
                })
                .when(full_width && has_trailing, |content| {
                    content.child(div().flex_grow(1.0))
                })
                .when_some(self.trailing, |content, build| {
                    content.child(
                        div()
                            .relative()
                            .when_some(icon_offset, |icon, offset| icon.top(offset))
                            .child(build(icon_foreground)),
                    )
                })
                .into_any_element()
        };

        self.core.render(
            style,
            ButtonLayout {
                icon_only: false,
                full_width,
                multiline,
            },
            content,
            window,
            cx,
        )
    }
}

/// A reusable icon-only action button.
///
/// The logical accessibility name is mandatory for this icon-only control. It prevents unnamed
/// actions and remains available when the control publishes a native accessibility node.
#[derive(IntoElement)]
pub struct IconButton {
    core: ButtonCore,
    icon: ContentBuilder,
}

impl IconButton {
    /// Pins only presentation for the development acceptance gallery.
    #[cfg(feature = "control-preview")]
    pub fn preview_state(mut self, state: crate::ControlPreviewState) -> Self {
        self.core.preview_state = Some(state);
        self
    }

    /// Supplies complete paints resolved for a contextual host surface, such as a Pane Caption.
    /// Interaction, disabled state, focus geometry, and metrics remain owned by the button.
    pub fn contextual_style(mut self, style: ButtonVariantStyle, focus_border: Rgba) -> Self {
        self.core.contextual_style = Some((style, focus_border));
        self
    }

    /// Creates a small secondary icon button with a mandatory logical accessibility name.
    pub fn new(
        id: impl Into<ElementId>,
        accessibility_name: impl Into<SharedString>,
        icon: impl FnOnce(Rgba) -> AnyElement + 'static,
    ) -> Self {
        Self {
            core: ButtonCore::new(id.into(), accessibility_name.into()),
            icon: Box::new(icon),
        }
    }

    /// Selects a bounded visual treatment from the installed button theme.
    pub fn variant(mut self, variant: ButtonVariant) -> Self {
        self.core.variant = variant;
        self
    }

    /// Selects a standard native control size.
    pub fn size(mut self, size: ButtonSize) -> Self {
        self.core.size = size;
        self
    }

    /// Overrides the outer pointer-target side length for a semantic mark with its own metrics.
    pub fn target_size(mut self, size: Pixels) -> Self {
        self.core.icon_button_size = Some(size.max(px(0.0)));
        self
    }

    /// Fits the control's rounded shape to its host's corner geometry.
    pub fn corner_radius(mut self, radius: Pixels) -> Self {
        self.core.corner_radius = Some(radius.max(px(0.0)));
        self
    }

    /// Paints an icon button inside its larger pointer target, as desktop window controls do.
    pub fn visual_inset(mut self, inset: Pixels) -> Self {
        self.core.visual_inset = inset.max(px(0.0));
        self
    }

    /// Overrides the border independently from the pointer target and visual fill.
    pub fn border_width(mut self, width: Pixels) -> Self {
        self.core.border_width = Some(width.max(px(0.0)));
        self
    }

    /// Uses a desktop outline instead of the application's AppKit focus band.
    pub fn focus_outline(mut self, width: Pixels, outset: Pixels, radius: Pixels) -> Self {
        self.core.focus_outline = Some((width.max(px(0.0)), outset, radius.max(px(0.0))));
        self
    }

    /// Selects the outer silhouette independently from visual emphasis.
    pub fn shape(mut self, shape: ButtonShape) -> Self {
        self.core.shape = shape;
        self
    }

    /// Assigns the semantic intent of the action.
    pub fn role(mut self, role: ButtonRole) -> Self {
        self.core.role = role;
        self
    }

    /// Controls whether the button can activate.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.core.disabled = disabled;
        self
    }

    /// Controls whether keyboard traversal may stop on this button.
    pub fn tab_stop(mut self, tab_stop: bool) -> Self {
        self.core.tab_stop = tab_stop;
        self
    }

    /// Controls whether the click that activates an inactive native window can invoke the action.
    pub fn accept_first_mouse(mut self, accept: bool) -> Self {
        self.core.accept_first_mouse = accept;
        self
    }

    /// Keeps ancestor hover presentation active while the pointer is over this button.
    ///
    /// The button still owns and consumes its pointer activation. Use this when the button is a
    /// child action whose containing interactive surface reveals it on hover.
    pub fn preserve_ancestor_hover(mut self) -> Self {
        self.core.preserve_ancestor_hover = true;
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

    /// Handles successful pointer or keyboard activation.
    pub fn on_activate(
        mut self,
        handler: impl Fn(&ButtonActivation, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.core.on_activate = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for IconButton {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let style = self.core.resolve_style(cx);
        self.core.render(
            style,
            ButtonLayout {
                icon_only: true,
                full_width: false,
                multiline: false,
            },
            move |_, icon_foreground| (self.icon)(icon_foreground),
            window,
            cx,
        )
    }
}

#[derive(Clone, Copy)]
struct ButtonLayout {
    icon_only: bool,
    full_width: bool,
    multiline: bool,
}

struct ButtonCore {
    id: ElementId,
    accessibility_name: SharedString,
    variant: ButtonVariant,
    size: ButtonSize,
    shape: ButtonShape,
    role: ButtonRole,
    disabled: bool,
    tab_stop: bool,
    debug_selector: Option<String>,
    tooltip: Option<Tooltip>,
    on_activate: Option<ActivationHandler>,
    injected_focus_handle: Option<FocusHandle>,
    parent_draws_focus_ring: bool,
    modal_borderless: bool,
    modal_press_owner: Option<ModalPressOwner>,
    preserve_ancestor_hover: bool,
    accept_first_mouse: bool,
    contextual_style: Option<(ButtonVariantStyle, Rgba)>,
    contextual_metrics: Option<ButtonMetrics>,
    icon_button_size: Option<Pixels>,
    corner_radius: Option<Pixels>,
    visual_inset: Pixels,
    border_width: Option<Pixels>,
    focus_outline: Option<(Pixels, Pixels, Pixels)>,
    joined_edge: JoinedEdge,
    even_spacing: bool,
    #[cfg(feature = "control-preview")]
    preview_state: Option<crate::ControlPreviewState>,
}

impl ButtonCore {
    fn new(id: ElementId, accessibility_name: SharedString) -> Self {
        Self {
            id,
            accessibility_name,
            variant: ButtonVariant::default(),
            size: ButtonSize::default(),
            shape: ButtonShape::default(),
            role: ButtonRole::Normal,
            disabled: false,
            tab_stop: false,
            debug_selector: None,
            tooltip: None,
            on_activate: None,
            injected_focus_handle: None,
            parent_draws_focus_ring: false,
            modal_borderless: false,
            modal_press_owner: None,
            preserve_ancestor_hover: false,
            accept_first_mouse: true,
            contextual_style: None,
            contextual_metrics: None,
            icon_button_size: None,
            corner_radius: None,
            visual_inset: px(0.0),
            border_width: None,
            focus_outline: None,
            joined_edge: JoinedEdge::None,
            even_spacing: false,
            #[cfg(feature = "control-preview")]
            preview_state: None,
        }
    }

    fn resolve_style(&self, cx: &App) -> ButtonStyle {
        let mut style = crate::floating_surface::hosted_button_theme(cx).resolve(
            self.variant,
            self.size,
            self.shape,
        );
        if let Some(metrics) = self.contextual_metrics {
            style = ButtonStyle::new(style.paints(), style.focus_border, metrics, self.shape);
        }
        if let Some((paints, focus_border)) = self.contextual_style {
            style.normal = paints.normal;
            style.hovered = paints.hovered;
            style.pressed = paints.pressed;
            style.disabled = paints.disabled;
            style.focus_border = focus_border;
        }
        if let Some(icon_button_size) = self.icon_button_size {
            style.icon_button_size = icon_button_size;
        }
        if let Some(corner_radius) = self.corner_radius {
            style.corner_radius = corner_radius;
        }
        if let Some(border_width) = self.border_width {
            style.border_width = border_width;
        }
        style
    }

    fn render(
        self,
        style: ButtonStyle,
        layout: ButtonLayout,
        build_content: impl FnOnce(Rgba, Rgba) -> AnyElement + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let font = crate::control_typography(cx).regular().clone();
        let enabled = !self.disabled && self.on_activate.is_some();
        let injected_focus_handle = self.injected_focus_handle.clone();
        let state = window.use_keyed_state(self.id.clone(), cx, move |window, cx| {
            ButtonState::new(injected_focus_handle, window, cx)
        });
        if !enabled && state.read(cx).focus_handle.is_focused(window) {
            window.blur(cx);
        }
        let modal_press_owner = self
            .modal_press_owner
            .clone()
            .or_else(ModalControlScope::current_press_owner);
        if let Some(owner) = &modal_press_owner {
            owner.register(&state, ButtonState::cancel_modal_owned_press, |state| {
                !state.interaction.has_owned_press()
            });
        }
        state.update(cx, |state, cx| {
            state.synchronize(enabled, self.tab_stop, cx);
        });

        let (focus_handle, pressed) = {
            let state = state.read(cx);
            (state.focus_handle.clone(), state.interaction.is_pressed())
        };
        let hover = crate::HoverFade::new(
            ElementId::NamedChild(std::sync::Arc::new(self.id.clone()), "hover".into()),
            window,
            cx,
        );
        let hover_level = hover.level(window, cx);
        let focus_anchor = ModalControlScope::register_current_focus_anchor(&focus_handle);
        let scroll_anchor = focus_anchor.as_ref().map(ModalFocusAnchor::scroll_anchor);
        let focused = focus_handle.is_focused(window);
        #[cfg(feature = "control-preview")]
        let (pressed, hover_level, focused) = self
            .preview_state
            .map(|state| {
                let hover_level = if state.hovered() { 1.0 } else { 0.0 };
                (state.pressed(), hover_level, state.focused())
            })
            .unwrap_or((pressed, hover_level, focused));
        let paint = resolve_paint(style, enabled, pressed, hover_level);
        let focus_ring = (focused && !self.parent_draws_focus_ring).then_some(style.focus_border);
        let border_color = if self.modal_borderless {
            paint.background
        } else {
            paint.border
        };

        let hover_state = state.clone();
        let down_state = state.clone();
        let move_state = state.clone();
        let up_state = state.clone();
        let exit_state = state.clone();
        let on_pointer_activate = self.on_activate.clone();
        let accept_first_mouse = self.accept_first_mouse;
        let role = self.role;
        let pointer_tracker = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, cx| {
                let hovered = hitbox.is_hovered(window);
                if hover_state.read(cx).interaction.is_hovered() != hovered {
                    let hover_state = hover_state.clone();
                    window.on_next_frame(move |_, cx| {
                        hover_state.update(cx, |state, cx| state.set_hovered(hovered, cx));
                    });
                }
                let down_hitbox = hitbox.clone();
                let move_hitbox = hitbox.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if !down_hitbox.is_hovered(window) {
                        return;
                    }
                    // Let transient overlays observe outside presses during capture, then keep
                    // secondary and middle presses from reaching an enclosing window drag region.
                    if event.button != MouseButton::Left {
                        if phase.bubble() {
                            cx.stop_propagation();
                        }
                        return;
                    }
                    if !phase.capture() {
                        return;
                    }
                    if event.first_mouse && !accept_first_mouse {
                        return;
                    }
                    window.prevent_default();
                    down_state.update(cx, |state, cx| state.pointer_down(cx));
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if !phase.capture() {
                        return;
                    }
                    move_state.update(cx, |state, cx| {
                        state.pointer_move(
                            move_hitbox.is_hovered(window),
                            event.pressed_button == Some(MouseButton::Left),
                            cx,
                        );
                    });
                });

                window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                    if !phase.capture() || event.button != MouseButton::Left {
                        return;
                    }
                    let was_armed = up_state.read(cx).interaction.is_pointer_armed();
                    if !was_armed {
                        return;
                    }
                    let activate = up_state.update(cx, |state, cx| {
                        state.pointer_up(hitbox.is_hovered(window), cx)
                    });
                    if activate && let Some(handler) = &on_pointer_activate {
                        handler(
                            &ButtonActivation {
                                source: ButtonActivationSource::Pointer,
                                role,
                            },
                            window,
                            cx,
                        );
                    }
                    cx.stop_propagation();
                });

                window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                    if phase.capture() {
                        exit_state.update(cx, |state, cx| state.mouse_exit(cx));
                    }
                });
            },
        )
        .absolute()
        .inset_0();

        let key_down_state = state.clone();
        let key_up_state = state;
        let on_keyboard_activate = self.on_activate.clone();
        let keyboard_focus = focus_handle.clone();
        let debug_selector = self.debug_selector;
        let focus_selector = debug_selector
            .as_ref()
            .map(|selector| format!("{selector}-keyboard-focus"))
            .unwrap_or_else(|| format!("{}-keyboard-focus", self.accessibility_name));
        let tooltip = self.tooltip;
        let content = build_content(paint.foreground, paint.icon_foreground);
        let preserve_ancestor_hover = self.preserve_ancestor_hover;

        let ring_id = crate::focus_ring::ring_id(&self.id);
        let corner_radii = self.joined_edge.corner_radii(style.corner_radius);
        let visual_inset = self.visual_inset;
        let button = div()
            .id(self.id)
            .debug_selector(move || {
                debug_selector.unwrap_or_else(|| self.accessibility_name.to_string())
            })
            .relative()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .when(!layout.multiline, |button| button.h(style.height))
            .when(layout.multiline, |button| {
                button.min_h(style.height).py(style.gap)
            })
            .when(layout.icon_only, |button| {
                button.h(style.icon_button_size).w(style.icon_button_size)
            })
            .when(!layout.icon_only, |button| {
                button.px(if self.even_spacing {
                    style.gap
                } else {
                    style.horizontal_padding
                })
            })
            .when(layout.full_width, |button| button.w_full())
            .rounded_tl(corner_radii.top_left)
            .rounded_tr(corner_radii.top_right)
            .rounded_br(corner_radii.bottom_right)
            .rounded_bl(corner_radii.bottom_left)
            .border(style.border_width)
            .when(self.joined_edge == JoinedEdge::Trailing, |button| {
                button.border_r(px(0.0))
            })
            .when(self.joined_edge == JoinedEdge::Leading, |button| {
                button.border_l(px(0.0))
            })
            .border_color(border_color)
            .when(visual_inset == px(0.0), |button| {
                button.bg(paint.background)
            })
            .when(visual_inset > px(0.0), |button| {
                button.child(
                    div()
                        .absolute()
                        .inset(visual_inset)
                        .rounded(style.corner_radius)
                        .bg(paint.background),
                )
            })
            .shadow(paint.shadow.layers())
            .shadow_outside_only()
            .text_color(paint.foreground)
            .text_size(style.font_size)
            .font(font)
            .cursor_default()
            .when(!preserve_ancestor_hover, |button| {
                button.block_mouse_except_scroll()
            })
            .track_focus(&focus_handle)
            .key_context(KEY_CONTEXT)
            .anchor_scroll(scroll_anchor)
            .on_action(move |_: &CaptureReturn, window, cx| {
                window.prevent_default();
                cx.propagate();
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let Some(key) = KeyboardActivation::from_key_down(event) else {
                    return;
                };
                window.prevent_default();
                if !event.is_held {
                    key_down_state.update(cx, |state, cx| state.keyboard_down(key, cx));
                }
                cx.stop_propagation();
            })
            .on_key_up(move |event: &KeyUpEvent, window, cx| {
                let Some(key) = KeyboardActivation::from_key(&event.keystroke.key) else {
                    return;
                };
                if !key_up_state.read(cx).interaction.owns_keyboard(key) {
                    return;
                }
                let may_activate =
                    !event.keystroke.modifiers.modified() && keyboard_focus.is_focused(window);
                let activate =
                    key_up_state.update(cx, |state, cx| state.keyboard_up(key, may_activate, cx));
                if activate && let Some(handler) = &on_keyboard_activate {
                    handler(
                        &ButtonActivation {
                            source: key.source(),
                            role,
                        },
                        window,
                        cx,
                    );
                }
                window.prevent_default();
                cx.stop_propagation();
            })
            .child(content)
            .child(pointer_tracker)
            .child(hover.tracker())
            .when_some(focus_anchor, |button, anchor| {
                button.child(anchor.bounds_tracker(style.border_width))
            });
        let button = crate::Ringed::new(
            button,
            focus_ring.map(|ring_color| {
                let mut ring =
                    crate::focus_ring(ring_id, ring_color, style.corner_radius, style.border_width)
                        .corner_radii(corner_radii)
                        .debug_selector(focus_selector);
                if let Some((width, outset, radius)) = self.focus_outline {
                    ring = ring.outline(width, outset, radius);
                }
                ring
            }),
        );

        if let Some(tooltip) = tooltip {
            tooltip
                .attach(button, TooltipTargetVisibility::Visible)
                .disabled(!enabled)
                .into_any_element()
        } else {
            button.into_any_element()
        }
    }
}

/// A press shows at once; hover eases between the resting and hovered paints.
fn resolve_paint(style: ButtonStyle, enabled: bool, pressed: bool, hover: f32) -> ButtonPaint {
    if !enabled {
        style.disabled
    } else if pressed {
        style.pressed
    } else {
        style.normal.mix(style.hovered, hover)
    }
}

struct ButtonState {
    focus_handle: FocusHandle,
    interaction: ButtonInteraction,
    enabled: bool,
}

impl ButtonState {
    fn new(
        injected_focus_handle: Option<FocusHandle>,
        window: &mut Window,
        cx: &mut gpui::Context<Self>,
    ) -> Self {
        let focus_handle = injected_focus_handle.unwrap_or_else(|| cx.focus_handle());
        cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify())
            .detach();
        cx.on_blur(&focus_handle, window, |state, _, cx| {
            state.interaction.cancel_keyboard();
            cx.notify();
        })
        .detach();
        cx.observe_window_activation(window, |state, window, cx| {
            if !window.is_window_active() && state.interaction.cancel_all() {
                cx.notify();
            }
        })
        .detach();
        Self {
            focus_handle,
            interaction: ButtonInteraction::default(),
            enabled: false,
        }
    }

    fn synchronize(&mut self, enabled: bool, tab_stop: bool, cx: &mut gpui::Context<Self>) {
        self.focus_handle = self.focus_handle.clone().tab_stop(enabled && tab_stop);
        if self.enabled != enabled {
            self.enabled = enabled;
            if !enabled && self.interaction.cancel_all() {
                cx.notify();
            }
        }
    }

    fn set_hovered(&mut self, hovered: bool, cx: &mut gpui::Context<Self>) {
        if self.interaction.set_hovered(hovered) {
            cx.notify();
        }
    }

    fn pointer_down(&mut self, cx: &mut gpui::Context<Self>) {
        if self.enabled && self.interaction.pointer_down() {
            cx.notify();
        }
    }

    fn pointer_move(&mut self, inside: bool, left_held: bool, cx: &mut gpui::Context<Self>) {
        if self.interaction.pointer_move(inside, left_held) {
            cx.notify();
        }
    }

    fn pointer_up(&mut self, inside: bool, cx: &mut gpui::Context<Self>) -> bool {
        let released_inside = self.interaction.pointer_up(inside);
        let activate = self.enabled && released_inside;
        cx.notify();
        activate
    }

    fn mouse_exit(&mut self, cx: &mut gpui::Context<Self>) {
        if self.interaction.mouse_exit() {
            cx.notify();
        }
    }

    fn cancel_modal_owned_press(&mut self, cx: &mut gpui::Context<Self>) {
        if self.interaction.cancel_all() {
            cx.notify();
        }
    }

    fn keyboard_down(&mut self, key: KeyboardActivation, cx: &mut gpui::Context<Self>) {
        if self.enabled && self.interaction.keyboard_down(key) {
            cx.notify();
        }
    }

    fn keyboard_up(
        &mut self,
        key: KeyboardActivation,
        focused: bool,
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let released_owned_press = self.interaction.keyboard_up(key);
        let activate = self.enabled && focused && released_owned_press;
        cx.notify();
        activate
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PointerPress {
    #[default]
    Idle,
    Armed {
        inside: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyboardActivation {
    Space,
    Return,
}

impl KeyboardActivation {
    fn from_key_down(event: &KeyDownEvent) -> Option<Self> {
        (!event.keystroke.modifiers.modified())
            .then(|| Self::from_key(&event.keystroke.key))
            .flatten()
    }

    fn from_key(key: &str) -> Option<Self> {
        match key {
            "space" => Some(Self::Space),
            "enter" => Some(Self::Return),
            _ => None,
        }
    }

    const fn source(self) -> ButtonActivationSource {
        match self {
            Self::Space => ButtonActivationSource::Space,
            Self::Return => ButtonActivationSource::Return,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ButtonInteraction {
    pointer: PointerPress,
    keyboard: Option<KeyboardActivation>,
    hovered: bool,
}

impl ButtonInteraction {
    fn has_owned_press(self) -> bool {
        self.is_pointer_armed() || self.keyboard.is_some()
    }

    fn is_pointer_armed(self) -> bool {
        matches!(self.pointer, PointerPress::Armed { .. })
    }

    fn is_pressed(self) -> bool {
        matches!(self.pointer, PointerPress::Armed { inside: true }) || self.keyboard.is_some()
    }

    fn is_hovered(self) -> bool {
        self.hovered
    }

    fn set_hovered(&mut self, hovered: bool) -> bool {
        let changed = self.hovered != hovered;
        self.hovered = hovered;
        changed
    }

    fn pointer_down(&mut self) -> bool {
        let changed = self.pointer != PointerPress::Armed { inside: true } || !self.hovered;
        self.pointer = PointerPress::Armed { inside: true };
        self.hovered = true;
        changed
    }

    fn pointer_move(&mut self, inside: bool, left_held: bool) -> bool {
        let hover_changed = self.set_hovered(inside);
        let PointerPress::Armed { inside: old_inside } = self.pointer else {
            return hover_changed;
        };
        if !left_held {
            self.pointer = PointerPress::Idle;
            return true;
        }
        if old_inside == inside {
            return hover_changed;
        }
        self.pointer = PointerPress::Armed { inside };
        true
    }

    fn pointer_up(&mut self, inside: bool) -> bool {
        let activate = matches!(self.pointer, PointerPress::Armed { .. }) && inside;
        self.pointer = PointerPress::Idle;
        self.hovered = inside;
        activate
    }

    fn cancel_pointer(&mut self) -> bool {
        let changed = self.pointer != PointerPress::Idle;
        self.pointer = PointerPress::Idle;
        changed
    }

    fn owns_keyboard(self, key: KeyboardActivation) -> bool {
        self.keyboard == Some(key)
    }

    fn keyboard_down(&mut self, key: KeyboardActivation) -> bool {
        if self.keyboard.is_some() {
            false
        } else {
            self.keyboard = Some(key);
            true
        }
    }

    fn keyboard_up(&mut self, key: KeyboardActivation) -> bool {
        let activate = self.owns_keyboard(key);
        if activate {
            self.keyboard = None;
        }
        activate
    }

    fn cancel_keyboard(&mut self) -> bool {
        let changed = self.keyboard.is_some();
        self.keyboard = None;
        changed
    }

    fn mouse_exit(&mut self) -> bool {
        let hover_changed = self.set_hovered(false);
        self.cancel_pointer() | hover_changed
    }

    fn cancel_all(&mut self) -> bool {
        self.cancel_pointer() | self.cancel_keyboard()
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use gpui::{
        Context, Entity, FocusHandle, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton,
        MouseExitEvent, Render, TestAppContext, VisualTestContext, Window, point, rgba,
    };

    use super::*;

    fn test_variant_style() -> ButtonVariantStyle {
        ButtonVariantStyle::new(
            ButtonPaint::new(rgba(0x101010ff), rgba(0xffffffff), rgba(0x202020ff)),
            ButtonPaint::new(rgba(0x303030ff), rgba(0xffffffff), rgba(0x404040ff)),
            ButtonPaint::new(rgba(0x505050ff), rgba(0xffffffff), rgba(0x606060ff)),
            ButtonPaint::new(rgba(0x707070ff), rgba(0x808080ff), rgba(0x909090ff)),
        )
    }

    fn test_theme() -> ButtonTheme {
        test_theme_with_focus(rgba(0x00aaffff))
    }

    fn test_theme_with_focus(focus_border: Rgba) -> ButtonTheme {
        let variant = test_variant_style();
        let metrics = ButtonMetrics::new(px(24.0));
        ButtonTheme::new(
            ButtonVariants::new(
                variant, variant, variant, variant, variant, variant, variant,
            ),
            ButtonSizes::new(metrics, metrics, metrics, metrics),
            focus_border,
        )
    }

    fn test_style() -> ButtonStyle {
        test_theme().resolve(
            ButtonVariant::Secondary,
            ButtonSize::Small,
            ButtonShape::Rounded,
        )
    }

    #[test]
    fn visual_state_precedence_should_be_disabled_pressed_hovered_then_normal() {
        let style = test_style();

        assert_eq!(resolve_paint(style, false, true, 1.0), style.disabled);
        assert_eq!(resolve_paint(style, true, true, 1.0), style.pressed);
        assert_eq!(resolve_paint(style, true, false, 1.0), style.hovered);
        assert_eq!(resolve_paint(style, true, false, 0.0), style.normal);
        let halfway = resolve_paint(style, true, false, 0.5).background();
        assert_eq!(
            halfway,
            crate::mix_rgba(style.normal.background(), style.hovered.background(), 0.5)
        );
    }

    #[test]
    fn theme_should_resolve_variant_size_and_shape_independently() {
        let base = test_variant_style();
        let destructive = ButtonVariantStyle::new(
            ButtonPaint::new(rgba(0xaa0000ff), rgba(0xffffffff), rgba(0xbb0000ff)),
            base.hovered,
            base.pressed,
            base.disabled,
        );
        let compact = ButtonMetrics::new(px(20.0)).corner_radius(px(4.0));
        let large = ButtonMetrics::new(px(40.0)).corner_radius(px(8.0));
        let theme = ButtonTheme::new(
            ButtonVariants::new(base, base, base, base, base, destructive, base),
            ButtonSizes::new(compact, compact, compact, large),
            rgba(0x00aaffff),
        );

        let rounded = theme.resolve(
            ButtonVariant::Destructive,
            ButtonSize::Large,
            ButtonShape::Rounded,
        );
        let square = theme.resolve(
            ButtonVariant::Destructive,
            ButtonSize::Large,
            ButtonShape::Square,
        );
        let capsule = theme.resolve(
            ButtonVariant::Destructive,
            ButtonSize::Large,
            ButtonShape::Capsule,
        );

        assert_eq!(rounded.normal, destructive.normal);
        assert_eq!(rounded.height, px(40.0));
        assert_eq!(rounded.corner_radius, px(8.0));
        assert_eq!(square.corner_radius, px(0.0));
        assert_eq!(capsule.corner_radius, px(20.0));
    }

    #[test]
    fn icon_target_override_does_not_change_or_density_scale_text_button_height() {
        let metrics = ButtonMetrics::new(px(20.0))
            .icon_button_size(px(28.0))
            .corner_radius(px(6.0));
        let theme = ButtonTheme::new(
            ButtonVariants::new(
                test_variant_style(),
                test_variant_style(),
                test_variant_style(),
                test_variant_style(),
                test_variant_style(),
                test_variant_style(),
                test_variant_style(),
            ),
            ButtonSizes::new(metrics, metrics, metrics, metrics),
            rgba(0x00aaffff),
        )
        .scaled_spacing(1.25);
        let style = theme.resolve(
            ButtonVariant::Secondary,
            ButtonSize::Compact,
            ButtonShape::Rounded,
        );

        assert_eq!(style.height, px(22.0));
        assert_eq!(style.icon_button_size, px(28.0));
        assert_eq!(style.corner_radius, px(6.0));
        assert_eq!(theme.icon_button_size(ButtonSize::Compact), px(28.0));
    }

    struct TooltipButtonsRoot {
        disabled: bool,
        activations: Rc<Cell<usize>>,
    }

    impl Render for TooltipButtonsRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let text_activations = self.activations.clone();
            let icon_activations = self.activations.clone();
            crate::TooltipLayer::new(
                div()
                    .flex()
                    .gap(px(24.0))
                    .child(
                        Button::new("button", "Button")
                            .debug_selector("tooltip-text-button")
                            .disabled(self.disabled)
                            .tooltip(
                                Tooltip::new("button-tooltip", "Button help")
                                    .debug_selector("button-help"),
                            )
                            .on_activate(move |_, _, _| {
                                text_activations.set(text_activations.get() + 1);
                            }),
                    )
                    .child(
                        IconButton::new("icon", "Icon", |_| div().into_any_element())
                            .debug_selector("tooltip-icon-button")
                            .disabled(self.disabled)
                            .tooltip(
                                Tooltip::new("icon-tooltip", "Icon help")
                                    .debug_selector("icon-help"),
                            )
                            .on_activate(move |_, _, _| {
                                icon_activations.set(icon_activations.get() + 1);
                            }),
                    ),
            )
        }
    }

    #[gpui::test]
    fn typed_tooltip_should_integrate_with_text_and_icon_buttons(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        cx.set_global(crate::TooltipTheme::new(
            crate::TooltipPaint::new(rgba(0xffffffff), rgba(0xaaaaaaff), rgba(0xccccccff)),
            crate::TooltipMetrics::new(px(320.0)),
        ));
        cx.update(crate::tooltip::init);
        let activations = Rc::new(Cell::new(0));
        let root_activations = activations.clone();
        let (root, cx) = cx.add_window_view(move |_, _| TooltipButtonsRoot {
            disabled: false,
            activations: root_activations,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        for (button, tooltip) in [
            ("tooltip-text-button", "button-help"),
            ("tooltip-icon-button", "icon-help"),
        ] {
            let center = cx
                .debug_bounds(button)
                .expect("the button renders")
                .center();
            cx.simulate_mouse_move(center, None, Modifiers::none());
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(500));
            cx.run_until_parked();
            assert!(cx.debug_bounds(tooltip).is_some(), "{tooltip} opens");
            let previous_activations = activations.get();
            cx.simulate_click(center, Modifiers::none());
            cx.run_until_parked();
            assert_eq!(activations.get(), previous_activations + 1);
            assert!(
                cx.debug_bounds(tooltip).is_none(),
                "activation dismisses {tooltip}"
            );
        }
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        for (button, tooltip) in [
            ("tooltip-text-button", "button-help"),
            ("tooltip-icon-button", "icon-help"),
        ] {
            cx.simulate_mouse_move(point(px(400.0), px(400.0)), None, Modifiers::none());
            let center = cx
                .debug_bounds(button)
                .expect("the disabled button renders")
                .center();
            cx.simulate_mouse_move(center, None, Modifiers::none());
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(500));
            cx.run_until_parked();
            assert!(
                cx.debug_bounds(tooltip).is_none(),
                "disabled {button} refuses hover"
            );
        }
    }

    #[gpui::test]
    fn icon_button_can_take_a_semantic_target_size_without_changing_its_size_role(
        cx: &mut TestAppContext,
    ) {
        struct IconTargetRoot;
        impl Render for IconTargetRoot {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                IconButton::new("icon", "Icon", |_| div().into_any_element())
                    .debug_selector("semantic-icon-target")
                    .size(ButtonSize::Compact)
                    .target_size(px(28.0))
            }
        }
        let variant = test_variant_style();
        let compact = ButtonMetrics::new(px(18.0)).corner_radius(px(3.0));
        let other = ButtonMetrics::new(px(40.0)).corner_radius(px(9.0));
        cx.set_global(ButtonTheme::new(
            ButtonVariants::new(
                variant, variant, variant, variant, variant, variant, variant,
            ),
            ButtonSizes::new(compact, other, other, other),
            rgba(0x00aaffff),
        ));
        let (_, cx) = cx.add_window_view(|_, _| IconTargetRoot);
        cx.run_until_parked();
        let bounds = cx
            .debug_bounds("semantic-icon-target")
            .expect("the icon target renders");
        assert_eq!(bounds.size, gpui::size(px(28.0), px(28.0)));
        let scale = cx.update(|window, _| window.scale_factor());
        let painted = cx.update(|window, _| {
            window
                .painted_quads()
                .into_iter()
                .find(|quad| {
                    quad.bounds == bounds.scale(scale) && !quad.background.is_transparent()
                })
                .expect("the icon target background paints")
        });
        assert_eq!(
            painted.corner_radii,
            gpui::Corners::all(px(3.0).scale(scale))
        );
    }

    #[test]
    fn pointer_reentry_should_restore_pressed_state_and_activate() {
        let mut state = ButtonInteraction::default();

        state.pointer_down();
        state.pointer_move(false, true);
        assert!(!state.is_pressed());
        state.pointer_move(true, true);

        assert!(state.is_pressed() && state.pointer_up(true));
    }

    #[test]
    fn lost_pointer_button_should_cancel_owned_press() {
        let mut state = ButtonInteraction::default();

        state.pointer_down();
        state.pointer_move(true, false);

        assert!(!state.is_pointer_armed());
    }

    #[test]
    fn repeated_keyboard_down_should_still_activate_once() {
        let mut state = ButtonInteraction::default();

        assert!(state.keyboard_down(KeyboardActivation::Return));
        assert!(!state.keyboard_down(KeyboardActivation::Return));
        assert!(state.keyboard_up(KeyboardActivation::Return));
        assert!(!state.keyboard_up(KeyboardActivation::Return));
    }

    #[test]
    fn cancelling_all_input_should_clear_pressed_state() {
        let mut state = ButtonInteraction::default();
        state.pointer_down();
        state.keyboard_down(KeyboardActivation::Space);

        state.cancel_all();

        assert!(!state.is_pressed());
    }

    struct PaintProbeRoot {
        icon_color: Rc<Cell<Rgba>>,
        disabled: bool,
    }

    #[test]
    fn ordinary_button_elevation_keeps_the_border_and_drops_nonresting_shadows() {
        let border = rgba(0x12345678);
        let shadow = crate::ControlShadow::single(crate::ControlShadowLayer::new(
            rgba(0x87654321).into(),
            px(0.0),
            px(1.0),
            px(2.0),
            px(-1.0),
        ));
        let disabled_border = test_theme()
            .paints(ButtonVariant::Secondary)
            .disabled
            .border;
        let paints = test_theme()
            .secondary_elevation(shadow, Some(border))
            .paints(ButtonVariant::Secondary);

        assert_eq!(
            [
                (paints.normal.border, paints.normal.shadow),
                (paints.hovered.border, paints.hovered.shadow),
                (paints.pressed.border, paints.pressed.shadow),
                (paints.disabled.border, paints.disabled.shadow),
            ],
            [
                (border, shadow),
                (border, shadow),
                (border, crate::ControlShadow::none()),
                (disabled_border, crate::ControlShadow::none()),
            ]
        );
    }
    impl Render for PaintProbeRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let observed = self.icon_color.clone();
            Button::new("paint-probe", "Label")
                .variant(ButtonVariant::Primary)
                .debug_selector("paint-probe")
                .disabled(self.disabled)
                .leading(move |color| {
                    observed.set(color);
                    div().into_any_element()
                })
                .on_activate(|_, _, _| {})
        }
    }

    struct RowProbe;

    impl Render for RowProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().w(px(200.0)).child(
                Button::new("row-probe", "Label")
                    .contextual_metrics(
                        ButtonMetrics::new(px(30.0))
                            .horizontal_padding(px(9.0))
                            .gap(px(7.0))
                            .border_width(px(0.0)),
                    )
                    .full_width(true)
                    .align_start()
                    .debug_selector("row-probe")
                    .leading(|_| {
                        div()
                            .debug_selector(|| "row-probe-icon".to_owned())
                            .size(px(10.0))
                            .into_any_element()
                    })
                    .on_activate(|_, _, _| {}),
            )
        }
    }

    #[gpui::test]
    fn contextual_metrics_shape_a_start_aligned_row_instead_of_the_standard_size(
        cx: &mut TestAppContext,
    ) {
        cx.set_global(test_theme());
        let (_, cx) = cx.add_window_view(|_, _| RowProbe);
        cx.run_until_parked();

        let button = cx.debug_bounds("row-probe").expect("button is rendered");
        let icon = cx.debug_bounds("row-probe-icon").expect("icon is rendered");
        let label = cx
            .debug_bounds("row-probe-label")
            .expect("label is rendered");
        assert_eq!(button.size, gpui::size(px(200.0), px(30.0)));
        assert_eq!(icon.left() - button.left(), px(9.0));
        assert_eq!(label.left() - icon.right(), px(7.0));
    }

    #[gpui::test]
    fn rendered_button_accessories_follow_each_state_icon_channel(cx: &mut TestAppContext) {
        let icons = [0x11223344, 0x55667788, 0x99aabbcc, 0x12345678].map(rgba);
        let base = test_variant_style();
        let paints = ButtonVariantStyle::new(
            base.normal.icon_foreground(icons[0]),
            base.hovered.icon_foreground(icons[1]),
            base.pressed.icon_foreground(icons[2]),
            base.disabled.icon_foreground(icons[3]),
        );
        let mut theme = test_theme();
        theme.variants.primary = paints;
        cx.set_global(theme);
        let observed = Rc::new(Cell::new(rgba(0)));
        let root_observed = observed.clone();
        let (root, cx) = cx.add_window_view(move |_, _| PaintProbeRoot {
            icon_color: root_observed,
            disabled: false,
        });
        cx.run_until_parked();
        assert_eq!(observed.get(), icons[0]);
        let center = cx
            .debug_bounds("paint-probe")
            .expect("button is rendered")
            .center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        crate::hover_fade::settle(cx);
        assert_eq!(observed.get(), icons[1]);
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        assert_eq!(observed.get(), icons[2]);
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(observed.get(), icons[3]);
    }

    #[gpui::test]
    fn hover_should_stay_cleared_after_the_pointer_leaves_the_window(cx: &mut TestAppContext) {
        let icons = [0x11223344, 0x55667788, 0x99aabbcc, 0x12345678].map(rgba);
        let base = test_variant_style();
        let mut theme = test_theme();
        theme.variants.primary = ButtonVariantStyle::new(
            base.normal.icon_foreground(icons[0]),
            base.hovered.icon_foreground(icons[1]),
            base.pressed.icon_foreground(icons[2]),
            base.disabled.icon_foreground(icons[3]),
        );
        cx.set_global(theme);
        let observed = Rc::new(Cell::new(rgba(0)));
        let root_observed = observed.clone();
        let (_, cx) = cx.add_window_view(move |_, _| PaintProbeRoot {
            icon_color: root_observed,
            disabled: false,
        });
        cx.run_until_parked();
        let center = cx
            .debug_bounds("paint-probe")
            .expect("button is rendered")
            .center();
        cx.simulate_mouse_move(center, None, Modifiers::none());
        crate::hover_fade::settle(cx);
        assert_eq!(observed.get(), icons[1]);

        // GPUI keeps the last pointer position after the pointer leaves, so later paints must not
        // read the button as hovered again.
        cx.simulate_event(MouseExitEvent {
            position: center,
            pressed_button: None,
            modifiers: Modifiers::none(),
        });
        crate::hover_fade::settle(cx);
        cx.update(|window, _| window.refresh());
        crate::hover_fade::settle(cx);
        assert_eq!(observed.get(), icons[0]);
    }

    struct TestRoot {
        activations: Rc<Cell<usize>>,
        last_source: Rc<Cell<Option<ButtonActivationSource>>>,
        disabled: bool,
        tab_stop: bool,
        overlay: bool,
        other_focus: FocusHandle,
    }

    struct ReturnTransferRoot {
        first_activations: Rc<Cell<usize>>,
        second_activations: Rc<Cell<usize>>,
    }

    struct NestedIconButtonRoot {
        ancestor_hovered: Rc<Cell<bool>>,
    }

    impl Render for NestedIconButtonRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let ancestor_hovered = Rc::clone(&self.ancestor_hovered);
            div()
                .id("hover-parent")
                .block_mouse_except_scroll()
                .on_hover(move |hovered, _, _| ancestor_hovered.set(*hovered))
                .child(
                    IconButton::new("nested-icon-button", "Nested action", |_| {
                        div().into_any_element()
                    })
                    .preserve_ancestor_hover()
                    .debug_selector("nested-icon-button")
                    .on_activate(|_, _, _| {}),
                )
        }
    }

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activations = self.activations.clone();
            let last_source = self.last_source.clone();
            div()
                .relative()
                .size_full()
                .child(div().track_focus(&self.other_focus).child("Other"))
                .child(
                    Button::new("test-button", "Activate")
                        .disabled(self.disabled)
                        .tab_stop(self.tab_stop)
                        .debug_selector("test-button")
                        .tooltip(Tooltip::new("test-button-tooltip", "Activate"))
                        .on_activate(move |activation, _, _| {
                            activations.set(activations.get() + 1);
                            last_source.set(Some(activation.source()));
                        }),
                )
                .when(self.overlay, |root| {
                    root.child(div().absolute().inset_0().occlude())
                })
        }
    }

    impl Render for ReturnTransferRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let first_activations = Rc::clone(&self.first_activations);
            let second_activations = Rc::clone(&self.second_activations);
            div()
                .child(
                    Button::new("return-transfer-first", "First")
                        .tab_stop(true)
                        .debug_selector("return-transfer-first")
                        .on_activate(move |_, _, _| {
                            first_activations.set(first_activations.get() + 1);
                        }),
                )
                .child(
                    Button::new("return-transfer-second", "Second")
                        .tab_stop(true)
                        .debug_selector("return-transfer-second")
                        .on_activate(move |_, _, _| {
                            second_activations.set(second_activations.get() + 1);
                        }),
                )
        }
    }

    type ButtonWindow<'a> = (
        Entity<TestRoot>,
        Rc<Cell<usize>>,
        Rc<Cell<Option<ButtonActivationSource>>>,
        &'a mut VisualTestContext,
    );

    fn button_window(cx: &mut TestAppContext, disabled: bool, tab_stop: bool) -> ButtonWindow<'_> {
        cx.update(super::init);
        cx.set_global(test_theme());
        let activations = Rc::new(Cell::new(0));
        let last_source = Rc::new(Cell::new(None));
        let root_activations = activations.clone();
        let root_source = last_source.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| TestRoot {
            activations: root_activations,
            last_source: root_source,
            disabled,
            tab_stop,
            overlay: false,
            other_focus: cx.focus_handle().tab_stop(true),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, activations, last_source, cx)
    }

    fn button_center(cx: &mut VisualTestContext) -> gpui::Point<Pixels> {
        cx.debug_bounds("test-button")
            .unwrap_or_else(|| panic!("button bounds were not painted"))
            .center()
    }

    #[gpui::test]
    fn nested_icon_button_should_preserve_ancestor_hover(cx: &mut TestAppContext) {
        cx.set_global(test_theme());
        let ancestor_hovered = Rc::new(Cell::new(false));
        let observed_hover = Rc::clone(&ancestor_hovered);
        let (_, cx) = cx.add_window_view(move |_, _| NestedIconButtonRoot {
            ancestor_hovered: observed_hover,
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("nested-icon-button")
            .expect("the nested icon button was not rendered");

        cx.simulate_mouse_move(button.center(), None, Modifiers::none());
        cx.run_until_parked();

        assert!(ancestor_hovered.get());
    }

    #[gpui::test]
    fn pointer_click_should_activate_exactly_once(cx: &mut TestAppContext) {
        let (_, activations, source, cx) = button_window(cx, false, false);
        let center = button_center(cx);

        cx.simulate_click(center, Modifiers::default());

        assert_eq!(activations.get(), 1);
        assert_eq!(source.get(), Some(ButtonActivationSource::Pointer));
    }

    #[gpui::test]
    fn pointer_release_outside_should_not_activate(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, false);
        let bounds = cx
            .debug_bounds("test-button")
            .unwrap_or_else(|| panic!("button bounds were not painted"));
        let outside = point(bounds.right() + px(20.0), bounds.bottom() + px(20.0));

        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn pointer_reentry_should_activate_once(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, false);
        let bounds = cx
            .debug_bounds("test-button")
            .unwrap_or_else(|| panic!("button bounds were not painted"));
        let outside = point(bounds.right() + px(20.0), bounds.bottom() + px(20.0));

        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());

        assert_eq!(activations.get(), 1);
    }

    #[gpui::test]
    fn disabled_button_should_not_activate(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, true, true);
        let center = button_center(cx);

        cx.simulate_click(center, Modifiers::default());

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn pointer_activation_should_preserve_existing_focus(cx: &mut TestAppContext) {
        let (root, _, _, cx) = button_window(cx, false, true);
        let other_focus = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, cx| other_focus.focus(window, cx));
        let center = button_center(cx);

        cx.simulate_click(center, Modifiers::default());

        assert!(cx.update(|window, _| other_focus.is_focused(window)));
    }

    #[gpui::test]
    fn focused_space_should_activate_on_key_up(cx: &mut TestAppContext) {
        let (_, activations, source, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });

        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
            prefer_character_input: false,
            is_held: false,
        });
        assert_eq!(activations.get(), 0);
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
        });

        assert_eq!(activations.get(), 1);
        assert_eq!(source.get(), Some(ButtonActivationSource::Space));
    }

    #[gpui::test]
    fn keyboard_focus_adds_one_outset_ring_when_focus_and_normal_colors_match(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = button_window(cx, false, true);
        cx.update(|_, cx| cx.set_global(test_theme_with_focus(rgba(0x202020ff))));
        root.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-button-keyboard-focus").is_none());

        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        cx.run_until_parked();
        let button = cx
            .debug_bounds("test-button")
            .expect("button should render");
        let focus = cx
            .debug_bounds("test-button-keyboard-focus")
            .expect("focused button should strengthen its single focus outline");
        let other_focus = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, cx| other_focus.focus(window, cx));
        cx.run_until_parked();

        assert!(
            focus.left() == button.left() - px(2.0)
                && focus.top() == button.top() - px(2.0)
                && focus.right() == button.right() + px(2.0)
                && focus.bottom() == button.bottom() + px(2.0)
                && cx.debug_bounds("test-button-keyboard-focus").is_none(),
            "button={button:?}, focus={focus:?}"
        );
    }

    #[gpui::test]
    fn focused_return_should_activate_once_on_key_up(cx: &mut TestAppContext) {
        let (_, activations, source, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });

        let enter = Keystroke::parse("enter").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: true,
        });
        assert_eq!(activations.get(), 0);
        cx.simulate_event(KeyUpEvent { keystroke: enter });

        assert_eq!(activations.get(), 1);
        assert_eq!(source.get(), Some(ButtonActivationSource::Return));
    }

    #[gpui::test]
    fn return_repeat_after_focus_transfer_should_not_activate_new_button(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.set_global(test_theme());
        let first_activations = Rc::new(Cell::new(0));
        let second_activations = Rc::new(Cell::new(0));
        let root_first_activations = Rc::clone(&first_activations);
        let root_second_activations = Rc::clone(&second_activations);
        let (_, cx) = cx.add_window_view(move |_, _| ReturnTransferRoot {
            first_activations: root_first_activations,
            second_activations: root_second_activations,
        });
        cx.update(|window, cx| {
            window.activate_window();
            window.focus_next(cx);
        });
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("return-transfer-first-keyboard-focus")
                .is_some()
        );
        let enter = Keystroke::parse("enter").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        cx.update(|window, cx| window.focus_next(cx));
        cx.run_until_parked();
        assert!(
            cx.debug_bounds("return-transfer-second-keyboard-focus")
                .is_some()
        );

        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: true,
        });
        cx.simulate_event(KeyUpEvent { keystroke: enter });

        assert_eq!(first_activations.get(), 0);
        assert_eq!(second_activations.get(), 0);
    }

    #[gpui::test]
    fn return_release_after_focus_change_should_not_activate(cx: &mut TestAppContext) {
        let (root, activations, _, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        let enter = Keystroke::parse("enter").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        let other_focus = root.read_with(cx, |root, _| root.other_focus.clone());
        cx.update(|window, cx| other_focus.focus(window, cx));

        cx.simulate_event(KeyUpEvent { keystroke: enter });

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn disabled_focused_button_should_not_arm_return(cx: &mut TestAppContext) {
        let (root, activations, _, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-button-keyboard-focus").is_some());
        let enter = Keystroke::parse("enter").unwrap_or_default();

        cx.simulate_event(KeyDownEvent {
            keystroke: enter.clone(),
            prefer_character_input: false,
            is_held: false,
        });
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();
        assert!(cx.update(|window, cx| window.focused(cx).is_none()));
        cx.simulate_event(KeyUpEvent {
            keystroke: enter.clone(),
        });
        root.update(cx, |root, cx| {
            root.disabled = false;
            cx.notify();
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("test-button-keyboard-focus").is_some());
        cx.simulate_event(KeyUpEvent { keystroke: enter });

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn disabling_a_focused_button_releases_responder_focus(cx: &mut TestAppContext) {
        let (root, _, _, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        assert!(cx.debug_bounds("test-button-keyboard-focus").is_some());

        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| window.focused(cx).is_none()));
    }

    #[gpui::test]
    fn modified_space_release_should_cancel_the_owned_keyboard_press(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, true);
        cx.update(|window, cx| {
            window.focus_next(cx);
            window.focus_next(cx);
        });
        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
            prefer_character_input: false,
            is_held: false,
        });

        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("shift-space").unwrap_or_default(),
        });
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
        });

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn occluding_overlay_should_block_pointer_activation(cx: &mut TestAppContext) {
        let (root, activations, _, cx) = button_window(cx, false, false);
        root.update(cx, |root, cx| {
            root.overlay = true;
            cx.notify();
        });
        cx.run_until_parked();
        let center = button_center(cx);

        cx.simulate_click(center, Modifiers::default());

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn disabling_during_pointer_press_should_cancel_activation(cx: &mut TestAppContext) {
        let (root, activations, _, cx) = button_window(cx, false, false);
        let center = button_center(cx);
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());
        root.update(cx, |root, cx| {
            root.disabled = true;
            cx.notify();
        });
        cx.run_until_parked();

        root.update(cx, |root, cx| {
            root.disabled = false;
            cx.notify();
        });
        cx.run_until_parked();

        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());

        assert_eq!(activations.get(), 0);
        cx.simulate_click(center, Modifiers::default());
        assert_eq!(activations.get(), 1);
    }

    #[gpui::test]
    fn window_deactivation_should_cancel_pointer_press(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, false);
        let center = button_center(cx);
        cx.simulate_mouse_down(center, MouseButton::Left, Modifiers::default());

        cx.deactivate_window();
        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());

        assert_eq!(activations.get(), 0);
    }
    #[gpui::test]
    fn chrome_button_preserving_ancestor_hover_consumes_secondary_activation(
        cx: &mut TestAppContext,
    ) {
        struct ChromeRoot(Rc<RefCell<Vec<crate::WindowDragRegionEvent>>>);
        impl Render for ChromeRoot {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let events = self.0.clone();
                crate::WindowDragRegion::new(
                    "chrome-drag-region",
                    "Move window",
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_start()
                        .child(
                            crate::IconButton::new("sidebar-toggle", "Toggle sidebar", |_| {
                                div().into_any_element()
                            })
                            .preserve_ancestor_hover()
                            .debug_selector("sidebar-toggle")
                            .on_activate(|_, _, _| {}),
                        )
                        .child(
                            crate::Menu::new(
                                "chrome-menu",
                                "Actions",
                                vec![crate::MenuEntry::action("Open", ())],
                            )
                            .debug_selector("chrome-menu-trigger")
                            .on_activate(|_, _, _| {}),
                        ),
                )
                .middle_activation(true)
                .on_event(move |event, _, _| events.borrow_mut().push(*event))
            }
        }
        cx.update(crate::menu::init);
        cx.set_global(test_theme());
        let metrics = crate::MenuMetrics::new(px(160.0), px(28.0));
        cx.set_global(crate::MenuTheme::new(
            crate::MenuPaint::new(
                rgba(0xffffffff),
                rgba(0xaaaaaaff),
                rgba(0x777777ff),
                rgba(0x336699ff),
                rgba(0xffffffff),
                rgba(0xff5555ff),
            ),
            crate::MenuSizes::new(metrics, metrics, metrics),
        ));
        let events = Rc::new(RefCell::new(Vec::new()));
        let (_, cx) = cx.add_window_view(|_, _| ChromeRoot(events.clone()));
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let button = cx.debug_bounds("sidebar-toggle").unwrap().center();
        for mouse_button in [MouseButton::Right, MouseButton::Middle] {
            cx.simulate_mouse_down(button, mouse_button, Modifiers::none());
            cx.simulate_mouse_up(button, mouse_button, Modifiers::none());
            assert!(events.borrow().is_empty());
            let trigger = cx.debug_bounds("chrome-menu-trigger").unwrap().center();
            cx.simulate_click(trigger, Modifiers::none());
            cx.run_until_parked();
            assert!(cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
            cx.simulate_mouse_down(button, mouse_button, Modifiers::none());
            cx.simulate_mouse_up(button, mouse_button, Modifiers::none());
            cx.run_until_parked();
            assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
            assert!(events.borrow().is_empty());
        }
        let empty = button + gpui::point(px(100.0), px(0.0));
        cx.simulate_mouse_down(empty, MouseButton::Right, Modifiers::none());
        assert_eq!(
            events.borrow().as_slice(),
            &[crate::WindowDragRegionEvent::SecondaryActivationRequested { position: empty }]
        );
    }
}
