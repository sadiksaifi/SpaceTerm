use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FocusHandle, Global, HitboxBehavior, InteractiveElement as _,
    IntoElement, KeyDownEvent, KeyUpEvent, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, RenderOnce, Rgba, SharedString,
    Styled as _, Window, canvas, div, prelude::FluentBuilder as _, px,
};

use crate::tooltip::Tooltip;

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
    Keyboard,
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
}

/// Paint values for one visual button state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonPaint {
    background: Rgba,
    foreground: Rgba,
    border: Rgba,
}

impl ButtonPaint {
    /// Creates paint values for a visual button state.
    pub fn new(background: Rgba, foreground: Rgba, border: Rgba) -> Self {
        Self {
            background,
            foreground,
            border,
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

    /// Returns the state's border color.
    pub fn border(self) -> Rgba {
        self.border
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
    horizontal_padding: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    font_size: Pixels,
}

impl ButtonMetrics {
    /// Creates metrics for a control height with compact native defaults.
    pub fn new(height: Pixels) -> Self {
        Self {
            height,
            horizontal_padding: px(8.0),
            gap: px(6.0),
            corner_radius: px(5.0),
            border_width: px(1.0),
            font_size: px(12.0),
        }
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
}

/// The complete set of visual variants required by the button API.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ButtonVariants {
    primary: ButtonVariantStyle,
    secondary: ButtonVariantStyle,
    outline: ButtonVariantStyle,
    ghost: ButtonVariantStyle,
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
        destructive: ButtonVariantStyle,
        link: ButtonVariantStyle,
    ) -> Self {
        Self {
            primary,
            secondary,
            outline,
            ghost,
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

    fn resolve(self, variant: ButtonVariant, size: ButtonSize, shape: ButtonShape) -> ButtonStyle {
        let variant = self.variants.resolve(variant);
        let metrics = self.sizes.resolve(size);
        ButtonStyle {
            normal: variant.normal,
            hovered: variant.hovered,
            pressed: variant.pressed,
            disabled: variant.disabled,
            focus_border: self.focus_border,
            height: metrics.height,
            horizontal_padding: metrics.horizontal_padding,
            gap: metrics.gap,
            corner_radius: match shape {
                ButtonShape::Rounded => metrics.corner_radius,
                ButtonShape::Square => px(0.0),
            },
            border_width: metrics.border_width,
            font_size: metrics.font_size,
        }
    }
}

impl Global for ButtonTheme {}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ButtonStyle {
    normal: ButtonPaint,
    hovered: ButtonPaint,
    pressed: ButtonPaint,
    disabled: ButtonPaint,
    focus_border: Rgba,
    height: Pixels,
    horizontal_padding: Pixels,
    gap: Pixels,
    corner_radius: Pixels,
    border_width: Pixels,
    font_size: Pixels,
}

type ActivationHandler = Rc<dyn Fn(&ButtonActivation, &mut Window, &mut App)>;
type ContentBuilder = Box<dyn FnOnce(Rgba) -> AnyElement>;

/// A reusable text action button with native desktop press semantics.
#[derive(IntoElement)]
pub struct Button {
    core: ButtonCore,
    label: SharedString,
    leading: Option<ContentBuilder>,
    trailing: Option<ContentBuilder>,
    full_width: bool,
}

impl Button {
    /// Creates a small secondary text button. Its label is also its logical accessibility name.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        let label = label.into();
        Self {
            core: ButtonCore::new(id.into(), label.clone()),
            label,
            leading: None,
            trailing: None,
            full_width: false,
        }
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
        let has_trailing = self.trailing.is_some();
        let content = move |foreground| {
            div()
                .flex()
                .items_center()
                .justify_center()
                .gap(style.gap)
                .when(full_width, |content| content.w_full())
                .when_some(self.leading, |content, build| {
                    content.child(build(foreground))
                })
                .child(div().line_height(gpui::relative(1.0)).child(self.label))
                .when(full_width && has_trailing, |content| {
                    content.child(div().flex_grow())
                })
                .when_some(self.trailing, |content, build| {
                    content.child(build(foreground))
                })
                .into_any_element()
        };

        self.core
            .render(style, false, full_width, content, window, cx)
    }
}

/// A reusable icon-only action button.
///
/// The logical accessibility name is mandatory even though GPUI 0.2.2 cannot yet publish custom
/// element roles and names to the native accessibility tree. Keeping the name in this interface
/// prevents unnamed icon controls and provides the semantic input for that framework seam.
#[derive(IntoElement)]
pub struct IconButton {
    core: ButtonCore,
    icon: ContentBuilder,
}

impl IconButton {
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
        self.core.render(style, true, false, self.icon, window, cx)
    }
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
        }
    }

    fn resolve_style(&self, cx: &App) -> ButtonStyle {
        cx.global::<ButtonTheme>()
            .resolve(self.variant, self.size, self.shape)
    }

    fn render(
        self,
        style: ButtonStyle,
        icon_only: bool,
        full_width: bool,
        build_content: impl FnOnce(Rgba) -> AnyElement + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let enabled = !self.disabled && self.on_activate.is_some();
        let state = window.use_keyed_state(self.id.clone(), cx, ButtonState::new);
        state.update(cx, |state, cx| {
            state.synchronize(enabled, self.tab_stop, cx);
        });

        let (focus_handle, pressed, hovered) = {
            let state = state.read(cx);
            (
                state.focus_handle.clone(),
                state.interaction.is_pressed(),
                state.interaction.is_hovered(),
            )
        };
        let focused = focus_handle.is_focused(window);
        let paint = resolve_paint(style, enabled, pressed, hovered);
        let border = if focused {
            style.focus_border
        } else {
            paint.border
        };

        let hover_state = state.clone();
        let down_state = state.clone();
        let move_state = state.clone();
        let up_state = state.clone();
        let exit_state = state.clone();
        let on_pointer_activate = self.on_activate.clone();
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
                    if !phase.capture()
                        || event.button != MouseButton::Left
                        || !down_hitbox.is_hovered(window)
                    {
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
        let tooltip = self.tooltip;
        let content = build_content(paint.foreground);

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
            .h(style.height)
            .when(icon_only, |button| button.w(style.height))
            .when(!icon_only, |button| button.px(style.horizontal_padding))
            .when(full_width, |button| button.w_full())
            .rounded(style.corner_radius)
            .border(style.border_width)
            .border_color(border)
            .bg(paint.background)
            .text_color(paint.foreground)
            .text_size(style.font_size)
            .cursor_default()
            .block_mouse_except_scroll()
            .track_focus(&focus_handle)
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                if !is_unmodified_space_down(event) {
                    return;
                }
                window.prevent_default();
                key_down_state.update(cx, |state, cx| state.space_down(cx));
                cx.stop_propagation();
            })
            .on_key_up(move |event: &KeyUpEvent, window, cx| {
                if event.keystroke.key != "space" || !key_up_state.read(cx).interaction.space_held {
                    return;
                }
                let may_activate =
                    !event.keystroke.modifiers.modified() && keyboard_focus.is_focused(window);
                let activate =
                    key_up_state.update(cx, |state, cx| state.space_up(may_activate, cx));
                if activate && let Some(handler) = &on_keyboard_activate {
                    handler(
                        &ButtonActivation {
                            source: ButtonActivationSource::Keyboard,
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
            .child(pointer_tracker);

        if let Some(tooltip) = tooltip {
            tooltip.attach(button).disabled(!enabled).into_any_element()
        } else {
            button.into_any_element()
        }
    }
}

fn resolve_paint(style: ButtonStyle, enabled: bool, pressed: bool, hovered: bool) -> ButtonPaint {
    if !enabled {
        style.disabled
    } else if pressed {
        style.pressed
    } else if hovered {
        style.hovered
    } else {
        style.normal
    }
}

fn is_unmodified_space_down(event: &KeyDownEvent) -> bool {
    event.keystroke.key == "space" && !event.keystroke.modifiers.modified()
}

struct ButtonState {
    focus_handle: FocusHandle,
    interaction: ButtonInteraction,
    enabled: bool,
}

impl ButtonState {
    fn new(window: &mut Window, cx: &mut gpui::Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        cx.on_blur(&focus_handle, window, |state, _, cx| {
            if state.interaction.cancel_keyboard() {
                cx.notify();
            }
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

    fn space_down(&mut self, cx: &mut gpui::Context<Self>) {
        if self.enabled && self.interaction.space_down() {
            cx.notify();
        }
    }

    fn space_up(&mut self, focused: bool, cx: &mut gpui::Context<Self>) -> bool {
        let released_owned_press = self.interaction.space_up();
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ButtonInteraction {
    pointer: PointerPress,
    space_held: bool,
    hovered: bool,
}

impl ButtonInteraction {
    fn is_pointer_armed(self) -> bool {
        matches!(self.pointer, PointerPress::Armed { .. })
    }

    fn is_pressed(self) -> bool {
        matches!(self.pointer, PointerPress::Armed { inside: true }) || self.space_held
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

    fn space_down(&mut self) -> bool {
        if self.space_held {
            false
        } else {
            self.space_held = true;
            true
        }
    }

    fn space_up(&mut self) -> bool {
        let activate = self.space_held;
        self.space_held = false;
        activate
    }

    fn cancel_keyboard(&mut self) -> bool {
        let changed = self.space_held;
        self.space_held = false;
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
        Render, TestAppContext, VisualTestContext, Window, point, rgba,
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
        let variant = test_variant_style();
        let metrics = ButtonMetrics::new(px(24.0));
        ButtonTheme::new(
            ButtonVariants::new(variant, variant, variant, variant, variant, variant),
            ButtonSizes::new(metrics, metrics, metrics, metrics),
            rgba(0x00aaffff),
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

        assert_eq!(resolve_paint(style, false, true, true), style.disabled);
        assert_eq!(resolve_paint(style, true, true, true), style.pressed);
        assert_eq!(resolve_paint(style, true, false, true), style.hovered);
        assert_eq!(resolve_paint(style, true, false, false), style.normal);
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
            ButtonVariants::new(base, base, base, base, destructive, base),
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

        assert_eq!(rounded.normal, destructive.normal);
        assert_eq!(rounded.height, px(40.0));
        assert_eq!(rounded.corner_radius, px(8.0));
        assert_eq!(square.corner_radius, px(0.0));
    }

    #[test]
    fn typed_tooltip_should_integrate_with_text_and_icon_buttons() {
        let button =
            Button::new("button", "Button").tooltip(Tooltip::new("button-tooltip", "Button help"));
        let icon = IconButton::new("icon", "Icon", |_| div().into_any_element())
            .tooltip(Tooltip::new("icon-tooltip", "Icon help"));

        assert!(button.core.tooltip.is_some() && icon.core.tooltip.is_some());
    }

    #[test]
    fn pointer_release_inside_should_activate() {
        let mut state = ButtonInteraction::default();

        state.pointer_down();

        assert!(state.pointer_up(true));
    }

    #[test]
    fn pointer_release_outside_should_cancel() {
        let mut state = ButtonInteraction::default();

        state.pointer_down();
        state.pointer_move(false, true);

        assert!(!state.pointer_up(false));
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
    fn repeated_space_down_should_still_activate_once() {
        let mut state = ButtonInteraction::default();

        assert!(state.space_down());
        assert!(!state.space_down());
        assert!(state.space_up());
        assert!(!state.space_up());
    }

    #[test]
    fn cancelling_all_input_should_clear_pressed_state() {
        let mut state = ButtonInteraction::default();
        state.pointer_down();
        state.space_down();

        state.cancel_all();

        assert!(!state.is_pressed());
    }

    struct TestRoot {
        activations: Rc<Cell<usize>>,
        last_source: Rc<Cell<Option<ButtonActivationSource>>>,
        disabled: bool,
        tab_stop: bool,
        overlay: bool,
        other_focus: FocusHandle,
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

    type ButtonWindow<'a> = (
        Entity<TestRoot>,
        Rc<Cell<usize>>,
        Rc<Cell<Option<ButtonActivationSource>>>,
        &'a mut VisualTestContext,
    );

    fn button_window(cx: &mut TestAppContext, disabled: bool, tab_stop: bool) -> ButtonWindow<'_> {
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
        cx.update(|window, _| other_focus.focus(window));
        let center = button_center(cx);

        cx.simulate_click(center, Modifiers::default());

        assert!(cx.update(|window, _| other_focus.is_focused(window)));
    }

    #[gpui::test]
    fn focused_space_should_activate_on_key_up(cx: &mut TestAppContext) {
        let (_, activations, source, cx) = button_window(cx, false, true);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });

        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
            is_held: false,
        });
        assert_eq!(activations.get(), 0);
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
        });

        assert_eq!(activations.get(), 1);
        assert_eq!(source.get(), Some(ButtonActivationSource::Keyboard));
    }

    #[gpui::test]
    fn enter_should_not_activate_focused_button(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, true);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });

        cx.simulate_keystrokes("enter");

        assert_eq!(activations.get(), 0);
    }

    #[gpui::test]
    fn modified_space_release_should_cancel_the_owned_keyboard_press(cx: &mut TestAppContext) {
        let (_, activations, _, cx) = button_window(cx, false, true);
        cx.update(|window, _| {
            window.focus_next();
            window.focus_next();
        });
        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
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

        cx.simulate_mouse_up(center, MouseButton::Left, Modifiers::default());

        assert_eq!(activations.get(), 0);
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
}
