use gpui::{
    AnyElement, App, AppContext as _, Bounds, Context, Element, ElementId, Entity, FocusHandle,
    GlobalElementId, Hitbox, HitboxBehavior, ImageSource, InspectorElementId,
    InteractiveElement as _, IntoElement, KeyBinding, KeyDownEvent, KeyUpEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseExitEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _,
    Pixels, RenderOnce, Rgba, ScrollHandle, ScrollWheelEvent, SharedString,
    StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, actions, canvas, div, img,
    prelude::FluentBuilder as _, px, relative, size,
};

use super::{
    ActionAxis, AlertAccessory, DialogSize, ModalActionEmphasis, ModalActionIntent,
    ModalActionRole, ModalActivationSource, ModalDesktopPolicy, ModalMetrics, ModalPaint,
    ModalPresentationId, ModalSurfaceGeometry, ModalTheme, ProgressState, TextDirection,
    clamp_surface_to_viewport,
    core::{
        ModalKind, ModalRenderAction, ModalRenderSnapshot, ModalWindowOwner, PreparedFocusIntent,
        PreparedModalSemantics, modal_owner_for_layer, register_root_scope,
        request_action_from_renderer, retire_window_owner, toggle_alert_suppression,
    },
    policy::{ActionArrangement, DefaultActionPresentation, is_safe_cancel, select_action_axis},
};
use crate::{
    Button, ButtonRole, ButtonSize, ButtonVariant, ControlHost, FloatingShell, Icon, IconName,
    OverlayScrollbar, OverlayScrollbarEvent, ProgressBar, ProgressSize, ScrollMetrics,
    button::{
        ModalControlScope, ModalFocusAnchorRegistry, ModalPressOwner,
        measure_button_intrinsic_width,
    },
};

const MODAL_KEY_CONTEXT: &str = "SpaceTermModal";
actions!(
    spaceterm_modal,
    [
        TraverseForward,
        TraverseBackward,
        ActivateDefault,
        ActivateCancel,
        ActivatePlatformCancel,
    ]
);

/// Platform-specific modal key equivalents layered over the portable modal bindings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModalKeybindingProfile {
    /// Conventional macOS Command-Period cancellation. Selecting this profile is explicit and
    /// performs no operating-system detection.
    MacOs,
    /// Linux desktops have no platform cancellation chord beyond the portable Escape.
    Linux,
}

/// Installs the platform-specific key equivalents for `profile`.
///
/// Applications explicitly install portable Tab, Shift-Tab, Return, and Escape behavior before
/// calling this function to opt into desktop-specific equivalents. Neither installation requires
/// host-platform detection in the reusable library.
pub fn install_modal_keybindings(cx: &mut App, profile: ModalKeybindingProfile) {
    match profile {
        ModalKeybindingProfile::MacOs => cx.bind_keys([KeyBinding::new(
            "cmd-.",
            ActivatePlatformCancel,
            Some(MODAL_KEY_CONTEXT),
        )]),
        ModalKeybindingProfile::Linux => {}
    }
}

/// Installs platform-neutral modal traversal, activation, and cancellation bindings.
pub fn install_portable_modal_keybindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", TraverseForward, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", TraverseBackward, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("enter", ActivateDefault, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("escape", ActivateCancel, Some(MODAL_KEY_CONTEXT)),
    ]);
}

#[cfg(test)]
pub(super) fn init(cx: &mut App) {
    install_portable_modal_keybindings(cx);
}

/// Final Operating-System Window layer for shared window-modal controls.
///
/// Place it around the complete root content. Tooltip dismissal is included. Supply complete
/// CommandPalette owners through [`Self::transient`] so the layer owns their placement above
/// ordinary content. The active modal is painted as the final normal
/// child rather than a deferred draw, allowing a modal-owned deferred Menu to remain above it. The
/// scrim blocks application pointer press, release, move, and wheel input without outside
/// dismissal or click-through. The modal key context blocks underlay keyboard routing while the
/// leading and trailing sentinels contain the complete current-frame GPUI tab-stop order.
///
/// This layer does not yet exclude the underlay from native accessibility traversal. Private
/// render snapshots and debug selectors support behavior tests; they provide no native
/// accessibility evidence.
#[derive(IntoElement)]
pub struct ModalLayer {
    content: AnyElement,
    transients: Vec<AnyElement>,
}

impl ModalLayer {
    /// Wraps complete Operating-System Window content.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
            transients: Vec::new(),
        }
    }

    /// Marks a pointer-only Operating-System Window management region in the underlay.
    ///
    /// The region keeps its pointer route and preserves modal focus. Wrap only window-management
    /// content, such as client Window Controls or an empty-titlebar pointer tracker. Later
    /// application siblings still occlude this region and remain blocked by the modal. Keyboard
    /// routing and modal focus containment are unchanged.
    pub fn window_chrome(content: impl IntoElement) -> impl IntoElement {
        super::window_chrome::ChromeRegion {
            content: content.into_any_element(),
            protect_from_resize: false,
        }
    }

    pub(crate) fn window_controls(content: impl IntoElement) -> impl IntoElement {
        super::window_chrome::ChromeRegion {
            content: content.into_any_element(),
            protect_from_resize: true,
        }
    }

    /// Inserts resize hitboxes with the painted Window Control bounds excluded.
    ///
    /// Call during prepaint inside this layer, before the modal overlay. The hitbox retains its
    /// pointer and cursor routes under a modal; the modal surface and later application siblings
    /// still occlude it. It blocks earlier application content from claiming the same press.
    /// Register only the geometry that performs window management.
    pub fn window_chrome_resize_hitboxes(
        bounds: Bounds<Pixels>,
        window: &mut Window,
    ) -> Vec<Hitbox> {
        super::window_chrome::insert_resize_hitboxes(bounds, window)
    }

    /// Presents a complete transient owner above ordinary content and below an active modal.
    ///
    /// Keeping the owner intact preserves its action routing. Its deferred child Menus remain
    /// above the normal surface without requiring callers to arrange paint-order siblings.
    pub fn transient(mut self, owner: impl IntoElement) -> Self {
        self.transients.push(owner.into_any_element());
        self
    }
}

impl RenderOnce for ModalLayer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let root = window.use_keyed_state("spaceterm-modal-root-scope", cx, ModalRootScope::new);
        let (root_focus, owner, chrome_frame) = root.read_with(cx, |root, _| {
            (
                root.focus.clone(),
                root.owner.clone(),
                root.chrome_frame.clone(),
            )
        });
        if !super::window_modal_is_open(window, cx) {
            chrome_frame.pointer.set(None);
        }
        register_root_scope(&owner, &root_focus, cx);

        super::window_chrome::ChromeScope {
            frame: chrome_frame,
            content: crate::TooltipLayer::new(
                div()
                    .id("spaceterm-modal-root")
                    .debug_selector(|| "spaceterm-modal-root".to_owned())
                    .relative()
                    .size_full()
                    .track_focus(&root_focus)
                    .child(self.content)
                    .children(self.transients)
                    .child(ModalOwnerView { owner }),
            )
            .into_any_element(),
        }
    }
}

struct ModalRootScope {
    chrome_frame: super::window_chrome::ChromeFrame,
    focus: FocusHandle,
    owner: gpui::Entity<ModalWindowOwner>,
}

impl ModalRootScope {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let owner = modal_owner_for_layer(window, cx);
        let press_owner = owner.read_with(cx, |owner, _| owner.press_owner());
        let chrome_frame = super::window_chrome::ChromeFrame::default();
        let chrome_pointer = chrome_frame.pointer.clone();
        cx.observe_window_activation(window, move |_, window, cx| {
            if !window.is_window_active() {
                press_owner.disarm(cx);
                chrome_pointer.set(None);
            }
        })
        .detach();
        cx.on_release(|state, cx| retire_window_owner(&state.owner, cx))
            .detach();
        Self {
            chrome_frame,
            focus: cx.focus_handle(),
            owner,
        }
    }
}

#[derive(IntoElement)]
struct ModalOwnerView {
    owner: gpui::Entity<ModalWindowOwner>,
}

impl RenderOnce for ModalOwnerView {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.owner
    }
}

pub(super) fn render_modal_owner(
    state: &mut ModalWindowOwner,
    snapshot: Option<ModalRenderSnapshot>,
    owner: WeakEntity<ModalWindowOwner>,
    window: &mut Window,
    cx: &mut Context<ModalWindowOwner>,
) -> AnyElement {
    let Some(snapshot) = snapshot else {
        return div().into_any_element();
    };
    let theme = super::modal_theme(cx);
    let policy = *cx.global::<ModalDesktopPolicy>();
    render_overlay(state, snapshot, owner, theme, policy, window, cx)
}

fn render_overlay(
    state: &mut ModalWindowOwner,
    snapshot: ModalRenderSnapshot,
    owner: WeakEntity<ModalWindowOwner>,
    theme: ModalTheme,
    policy: ModalDesktopPolicy,
    window: &mut Window,
    cx: &mut Context<ModalWindowOwner>,
) -> AnyElement {
    let metrics = theme.metrics;
    let paint = theme.paint;
    let shell = theme.shell;
    let typography = crate::control_typography(cx);
    let content_viewport = crate::content_viewport(window);
    let viewport = content_viewport.size;
    let desired_width = metrics.width_for(match snapshot.kind {
        ModalKind::Alert => DialogSize::Regular,
        ModalKind::Dialog => snapshot.dialog_size,
        ModalKind::Progress => DialogSize::Compact,
    });
    let height_cap = metrics.maximum_height().min(match snapshot.kind {
        ModalKind::Alert => metrics.alert_height_cap(),
        ModalKind::Dialog => metrics.dialog_height_cap(),
        ModalKind::Progress => metrics.progress_height_cap(),
    });
    let mut geometry =
        clamp_surface_to_viewport(viewport, size(desired_width, height_cap), metrics);
    geometry.origin_x += content_viewport.origin.x;
    geometry.origin_y += content_viewport.origin.y;
    let available_actions = (geometry.size.width - metrics.surface_padding * 2.0).max(px(1.0));
    // Action layout is chosen before the shell mounts its Floating control host.
    let button_theme = ControlHost::Floating.button_theme(cx);
    let measured_widths = snapshot
        .actions
        .iter()
        .map(|action| {
            measure_button_intrinsic_width(
                button_theme,
                &action.label,
                ButtonSize::Small,
                window,
                cx,
            )
        })
        .collect::<Vec<_>>();
    let axis = select_action_axis(
        geometry.size.width,
        available_actions,
        &measured_widths,
        metrics,
    );
    let arrangement = policy.action_arrangement(&snapshot.actions, axis);

    let state_id: SharedString = format!(
        "modal-focus-ring-{}-{}",
        snapshot.id.as_str(),
        snapshot.presentation.value()
    )
    .into();
    let suppression_available = matches!(
        &snapshot.semantics,
        PreparedModalSemantics::Alert {
            suppression: Some(_),
            ..
        }
    );
    let focus_state = window.use_keyed_state(state_id, cx, ModalFocusRing::new);
    focus_state.update(cx, |state, cx| {
        state.presentation = Some(snapshot.presentation);
        state.synchronize(
            &snapshot.actions,
            &snapshot.focus_intent,
            snapshot.focus_request_generation,
            suppression_available,
            window,
            cx,
        );
    });
    let (
        scope,
        surface_focus,
        leading,
        trailing,
        suppression_focus,
        action_focus,
        body_scroll,
        body_scrollbar,
        footer_scroll,
        body_focus_anchors,
        footer_focus_anchors,
    ) = {
        let state = focus_state.read(cx);
        (
            state.scope.clone(),
            state.surface.clone(),
            state.leading.clone(),
            state.trailing.clone(),
            state.suppression.clone(),
            state.action_focus.clone(),
            state.body_scroll.clone(),
            state.body_scrollbar.clone(),
            state.footer_scroll.clone(),
            state.body_focus_anchors.clone(),
            state.footer_focus_anchors.clone(),
        )
    };
    state.register_modal_scope(snapshot.presentation, &scope);
    schedule_focus_reconciliation(focus_state.clone(), window, cx);

    let press_owner = state.press_owner();
    let blocker = render_blocker(geometry, press_owner.clone());
    let header = render_header(
        &snapshot,
        geometry.size.height * metrics.header_maximum_fraction(),
        metrics,
        paint,
        shell,
        typography.heading().clone(),
    );
    let suppression_is_focused = suppression_focus.is_focused(window);
    let body = render_body(
        &snapshot,
        owner.clone(),
        suppression_focus,
        suppression_is_focused,
        press_owner.clone(),
        body_scroll,
        body_scrollbar,
        body_focus_anchors,
        metrics,
        paint,
        shell,
        policy.text_direction(),
        window,
        cx,
    );
    let footer = render_footer(
        &snapshot,
        owner.clone(),
        axis,
        arrangement,
        policy,
        policy.text_direction(),
        action_focus,
        press_owner,
        footer_scroll,
        footer_focus_anchors,
        geometry.size.height * metrics.footer_maximum_fraction(),
        metrics,
        shell,
    );
    let presentation = snapshot.presentation;
    let default_action = enabled_action(snapshot.default_action, &snapshot.actions);
    let cancel_action = safe_cancel_action(snapshot.cancel_action, &snapshot.actions);
    let forward_focus = focus_state.clone();
    let default_focus = focus_state.clone();
    let backward_focus = focus_state;
    let default_owner = owner.clone();
    let cancel_owner = owner.clone();
    let platform_cancel_owner = owner;
    let press_scope = scope.clone();

    let surface = div()
        .id(("modal-surface", presentation.value()))
        .debug_selector(move || format!("modal-surface-{}", presentation.value()))
        .absolute()
        .left(geometry.origin_x)
        .top(geometry.origin_y)
        .w(geometry.size.width)
        .max_h(geometry.size.height)
        .min_w_0()
        .min_h_0()
        // The surface owns pointer input even where it covers a window-management region.
        .occlude()
        .flex()
        .flex_col()
        .text_color(paint.primary_text)
        .font(typography.regular().clone())
        .track_focus(&scope)
        // Static content accepts no keyboard focus. A press that no control claims would
        // otherwise focus the containment scope, which focus repair moves to the first tab stop.
        .on_any_mouse_down(move |_, window, cx| {
            if press_scope.contains_focused(window, cx) {
                window.prevent_default();
            }
        })
        .key_context(MODAL_KEY_CONTEXT)
        .on_action(move |_: &TraverseForward, window, cx| {
            if !super::window_has_owned_popup(window, cx) {
                forward_focus.update(cx, |state, cx| state.focus_next(window, cx));
            }
            cx.stop_propagation();
        })
        .on_action(move |_: &TraverseBackward, window, cx| {
            if !super::window_has_owned_popup(window, cx) {
                backward_focus.update(cx, |state, cx| state.focus_previous(window, cx));
            }
            cx.stop_propagation();
        })
        .on_action(move |_: &ActivateDefault, window, cx| {
            if default_focus.read(cx).has_focused_action(window) {
                window.prevent_default();
                cx.propagate();
                return;
            }
            if let Some(index) = default_action {
                request_action_from_renderer(
                    &default_owner,
                    presentation,
                    index,
                    ModalActivationSource::Return,
                    cx,
                );
            }
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_action(move |_: &ActivateCancel, window, cx| {
            if super::window_has_owned_popup(window, cx) {
                return;
            }
            if let Some(index) = cancel_action {
                request_action_from_renderer(
                    &cancel_owner,
                    presentation,
                    index,
                    ModalActivationSource::Escape,
                    cx,
                );
            }
            window.prevent_default();
            cx.stop_propagation();
        })
        .on_action(move |_: &ActivatePlatformCancel, window, cx| {
            if let Some(index) = cancel_action {
                request_action_from_renderer(
                    &platform_cancel_owner,
                    presentation,
                    index,
                    ModalActivationSource::CommandPeriod,
                    cx,
                );
            }
            window.prevent_default();
            cx.stop_propagation();
        })
        .child(div().size_0().track_focus(&surface_focus))
        .child(div().size_0().track_focus(&leading))
        .child(header)
        .child(body)
        .when(!snapshot.actions.is_empty(), |surface| {
            surface.child(footer)
        })
        .child(div().size_0().track_focus(&trailing));

    // The shell owns the modal's material, edge, corners, and elevation, and hosts every control
    // resting on it so nested fields and buttons compose against this surface. Tooltip ancestry
    // wraps the mounted surface so a tooltip raised from any descendant stays tied to this
    // presentation through layout, prepaint, and paint.
    div()
        .id(("modal-overlay", presentation.value()))
        .absolute()
        .inset_0()
        .child(div().absolute().inset_0().bg(theme.scrim))
        .child(blocker)
        .child(crate::tooltip::ModalTooltipScope::new(
            shell.mount(surface),
            super::ModalParentToken {
                window_id: window.window_handle().window_id(),
                presentation,
            },
        ))
        .into_any_element()
}

fn render_blocker(geometry: ModalSurfaceGeometry, press_owner: ModalPressOwner) -> AnyElement {
    canvas(
        move |bounds, window, _| {
            let chrome = super::window_chrome::prepaint_blocker(bounds, window);
            super::window_chrome::set_blocker(ModalPointerBlocker {
                geometry,
                press_owner,
                chrome,
            });
        },
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

pub(super) struct ModalPointerBlocker {
    geometry: ModalSurfaceGeometry,
    press_owner: ModalPressOwner,
    chrome: super::window_chrome::ChromeRouting,
}

impl ModalPointerBlocker {
    // Register before painting the underlay. GPUI runs capture handlers from back to front,
    // so application controls must encounter this gate before they can claim a chrome press.
    pub(super) fn register(self, window: &mut Window) {
        let Self {
            geometry,
            press_owner,
            chrome,
        } = self;
        let chrome = std::rc::Rc::new(chrome);
        let down_chrome = chrome.clone();
        let up_chrome = chrome.clone();
        let down_owner = press_owner.clone();
        let up_owner = press_owner.clone();
        let move_owner = press_owner.clone();
        let exit_pointer = chrome.pointer.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if !phase.capture() {
                return;
            }
            down_chrome.pointer.set(None);
            if !surface_contains(geometry, event.position) {
                down_owner.disarm(cx);
                window.prevent_default();
                if down_chrome.contains(event.position, window) {
                    down_chrome.pointer.set(Some(event.button));
                } else {
                    cx.stop_propagation();
                }
            }
        });
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if !phase.capture() {
                return;
            }
            if up_chrome.pointer.get() == Some(event.button) {
                up_chrome.pointer.set(None);
                return;
            }
            if !surface_contains(geometry, event.position) {
                up_owner.disarm(cx);
                window.prevent_default();
                cx.stop_propagation();
            }
        });
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase.capture() && !surface_contains(geometry, event.position) {
                move_owner.disarm(cx);
                if chrome.pointer.get().is_some() && chrome.pointer.get() == event.pressed_button {
                    return;
                }
                if event.pressed_button.is_none() && chrome.contains(event.position, window) {
                    return;
                }
                window.prevent_default();
                cx.stop_propagation();
            }
        });
        window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase.capture() && !surface_contains(geometry, event.position) {
                press_owner.disarm(cx);
                window.prevent_default();
                cx.stop_propagation();
            }
        });
        window.on_mouse_event(move |_: &MouseExitEvent, phase, _, _| {
            if phase.capture() {
                exit_pointer.set(None);
            }
        });
    }
}

fn surface_contains(geometry: ModalSurfaceGeometry, point: gpui::Point<gpui::Pixels>) -> bool {
    point.x >= geometry.origin_x
        && point.x <= geometry.origin_x + geometry.size.width
        && point.y >= geometry.origin_y
        && point.y <= geometry.origin_y + geometry.size.height
}

#[derive(Clone, Copy, Debug)]
struct AlertIntentPresentation {
    icon: IconName,
    selector: &'static str,
    accent: Rgba,
}

fn alert_intent_presentation(
    intent: super::AlertIntent,
    paint: ModalPaint,
) -> AlertIntentPresentation {
    match intent {
        super::AlertIntent::Informational => AlertIntentPresentation {
            icon: IconName::Info,
            selector: "informational",
            accent: paint.informational,
        },
        super::AlertIntent::Warning => AlertIntentPresentation {
            icon: IconName::TriangleAlert,
            selector: "warning",
            accent: paint.warning,
        },
        super::AlertIntent::Critical => AlertIntentPresentation {
            icon: IconName::CircleAlert,
            selector: "critical",
            accent: paint.critical,
        },
    }
}

fn render_header(
    snapshot: &ModalRenderSnapshot,
    maximum_height: gpui::Pixels,
    metrics: ModalMetrics,
    paint: ModalPaint,
    shell: FloatingShell,
    heading_font: gpui::Font,
) -> AnyElement {
    let (title, description) = match &snapshot.semantics {
        PreparedModalSemantics::Alert { visible_title, .. } => (visible_title.clone(), None),
        PreparedModalSemantics::Dialog {
            visible_title,
            description,
            ..
        } => (visible_title.clone(), description.clone()),
        PreparedModalSemantics::Progress { visible_title, .. } => (visible_title.clone(), None),
    };
    div()
        .id(("modal-header", snapshot.presentation.value()))
        .debug_selector(|| "modal-header".to_owned())
        .flex_shrink_0()
        .min_w_0()
        .min_h_0()
        .max_h(maximum_height)
        .overflow_x_hidden()
        .overflow_y_scroll()
        .px(metrics.surface_padding)
        .pt(metrics.surface_padding)
        .pb(metrics.section_gap)
        .border_b(shell.hairline())
        .border_color(shell.divider())
        .child(
            div()
                .debug_selector(|| "modal-header-title".to_owned())
                .min_w_0()
                .text_size(metrics.title_size)
                .text_color(paint.primary_text)
                .font(heading_font)
                .whitespace_normal()
                .child(title),
        )
        .when_some(description, |header, description| {
            header.child(
                div()
                    .debug_selector(|| "modal-header-description".to_owned())
                    .min_w_0()
                    .mt(metrics.action_gap)
                    .text_size(metrics.detail_size)
                    .text_color(paint.secondary_text)
                    .whitespace_normal()
                    .child(description),
            )
        })
        .into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "one private body renderer consumes resolved modal ownership, focus, paint, and context"
)]
fn render_body(
    snapshot: &ModalRenderSnapshot,
    owner: WeakEntity<ModalWindowOwner>,
    suppression_focus: FocusHandle,
    suppression_is_focused: bool,
    press_owner: ModalPressOwner,
    body_scroll: ScrollHandle,
    body_scrollbar: Entity<OverlayScrollbar<f32>>,
    body_focus_anchors: ModalFocusAnchorRegistry,
    metrics: ModalMetrics,
    paint: ModalPaint,
    shell: FloatingShell,
    direction: TextDirection,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    // The body scrolls like every other scrolling region: the shared overlay scrollbar shows its
    // extent while it moves, without reserving a gutter.
    let scroll_metrics = body_scroll_metrics(&body_scroll);
    body_scrollbar.update(cx, |scrollbar, cx| scrollbar.sync(scroll_metrics, cx));
    let content = match &snapshot.semantics {
        PreparedModalSemantics::Alert {
            message,
            detail,
            intent,
            accessory,
            suppression,
            ..
        } => {
            let accessory = accessory.as_ref().map(|accessory| {
                let extent = metrics.accessory_extent();
                match accessory {
                    AlertAccessory::Icon {
                        image: Some(image), ..
                    }
                    | AlertAccessory::Media { image, .. } => {
                        img(ImageSource::Render(image.clone()))
                            .size(extent)
                            .into_any_element()
                    }
                    AlertAccessory::Icon { image: None, .. } => {
                        Icon::new(IconName::ImageOff, extent, paint.secondary_text)
                            .into_any_element()
                    }
                }
            });
            let intent_presentation = alert_intent_presentation(*intent, paint);
            let intent_selector = format!("modal-alert-intent-{}", intent_presentation.selector);
            let marker_selector =
                format!("modal-alert-intent-mark-{}", intent_presentation.selector);
            let detail_selector = format!("modal-alert-detail-{}", snapshot.presentation.value());
            let marker_extent = metrics.accessory_extent() / 2.0;
            let message_panel = div()
                .debug_selector(move || intent_selector.clone())
                .flex()
                .items_start()
                .min_w_0()
                .gap(metrics.action_gap / 2.0)
                .child(
                    div()
                        .debug_selector(move || marker_selector.clone())
                        .size(marker_extent)
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(
                            intent_presentation.icon,
                            marker_extent * 0.8,
                            intent_presentation.accent,
                        )),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .debug_selector(|| "modal-alert-message".to_owned())
                                .text_size(metrics.body_size)
                                .whitespace_normal()
                                .child(message.clone()),
                        )
                        .when_some(detail.clone(), |content, detail| {
                            let detail_selector = detail_selector.clone();
                            content.child(
                                div()
                                    .debug_selector(move || detail_selector.clone())
                                    .mt(metrics.action_gap / 2.0)
                                    .text_size(metrics.detail_size)
                                    .text_color(paint.secondary_text)
                                    .whitespace_normal()
                                    .child(detail),
                            )
                        }),
                );
            let suppression = suppression.clone();
            let presentation = snapshot.presentation;
            let suppression_enabled = snapshot.interaction_enabled;
            div()
                .flex()
                .flex_col()
                .gap(metrics.section_gap)
                .when_some(accessory, |body, accessory| {
                    body.child(
                        div()
                            .size(metrics.accessory_extent())
                            .overflow_hidden()
                            .child(accessory),
                    )
                })
                .child(message_panel)
                .when_some(suppression, move |body, (label, selected)| {
                    body.child(render_alert_suppression(
                        label,
                        selected,
                        presentation,
                        suppression_enabled,
                        owner,
                        suppression_focus,
                        suppression_is_focused,
                        press_owner,
                        body_focus_anchors.clone(),
                        metrics,
                        shell,
                        window,
                        cx,
                    ))
                })
                .into_any_element()
        }
        PreparedModalSemantics::Dialog { .. } => ModalControlScopeElement {
            content: snapshot
                .body
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
            controls: ModalControlScope::new(press_owner.clone())
                .with_focus_anchors(body_focus_anchors.clone()),
        }
        .into_any_element(),
        PreparedModalSemantics::Progress { .. } => {
            let progress = snapshot.progress.as_ref();
            let status = progress
                .map(|progress| progress.status.clone())
                .unwrap_or_default();
            let detail = progress.and_then(|progress| progress.detail.clone());
            let progress_state = progress.map(|progress| progress.progress);
            div()
                .flex()
                .flex_col()
                .min_w_0()
                .gap(metrics.section_gap)
                .child(
                    div()
                        .id(("modal-progress-status", snapshot.presentation.value()))
                        .debug_selector(|| "modal-progress-status".to_owned())
                        .h(metrics.progress_status_region_height())
                        .flex_shrink_0()
                        .min_w_0()
                        .overflow_x_hidden()
                        .overflow_y_scroll()
                        .text_size(metrics.body_size)
                        .whitespace_normal()
                        .child(status.clone()),
                )
                .child(
                    ProgressBar::new(
                        ("modal-progress", snapshot.presentation.value()),
                        status,
                        progress_state.unwrap_or(ProgressState::Indeterminate),
                    )
                    .size(ProgressSize::Regular)
                    .right_to_left(direction == TextDirection::RightToLeft)
                    .debug_selector("modal-progress"),
                )
                .child(
                    div()
                        .id(("modal-progress-detail", snapshot.presentation.value()))
                        .debug_selector(|| "modal-progress-detail".to_owned())
                        .h(metrics.progress_detail_region_height())
                        .flex_shrink_0()
                        .min_w_0()
                        .overflow_x_hidden()
                        .overflow_y_scroll()
                        .text_size(metrics.detail_size)
                        .text_color(paint.secondary_text)
                        .whitespace_normal()
                        .when_some(detail, |region, detail| region.child(detail)),
                )
                .into_any_element()
        }
    };

    let revealing = body_scrollbar.downgrade();
    let revealed_scroll = body_scroll.clone();
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(
            div()
                .id(("modal-body", snapshot.presentation.value()))
                .debug_selector(|| "modal-body-viewport".to_owned())
                .flex_1()
                .min_h_0()
                .min_w_0()
                .overflow_x_hidden()
                .overflow_y_scroll()
                .track_scroll(&body_scroll)
                .on_scroll_wheel(move |_, _, cx| {
                    let metrics = body_scroll_metrics(&revealed_scroll);
                    let _ = revealing.update(cx, |scrollbar, cx| scrollbar.reveal(metrics, cx));
                })
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .p(metrics.surface_padding)
                        .child(content),
                ),
        )
        .child(body_scrollbar)
        .into_any_element()
}

fn body_scroll_metrics(scroll: &ScrollHandle) -> Option<ScrollMetrics<f32>> {
    ScrollMetrics::for_pixels(
        0.0,
        f32::from(scroll.bounds().size.height),
        f32::from(scroll.max_offset().y),
        -f32::from(scroll.offset().y),
    )
}

struct ModalControlScopeElement {
    content: AnyElement,
    controls: ModalControlScope,
}

impl IntoElement for ModalControlScopeElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ModalControlScopeElement {
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
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (
            self.controls
                .enter(|| self.content.request_layout(window, cx)),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.controls.enter(|| self.content.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.controls.enter(|| self.content.paint(window, cx));
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one private suppression renderer consumes modal ownership, focus, geometry, and paint"
)]
fn render_alert_suppression(
    label: SharedString,
    selected: bool,
    presentation: ModalPresentationId,
    enabled: bool,
    owner: WeakEntity<ModalWindowOwner>,
    focus: FocusHandle,
    focused: bool,
    press_owner: ModalPressOwner,
    focus_anchors: ModalFocusAnchorRegistry,
    metrics: ModalMetrics,
    shell: FloatingShell,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let state_id: SharedString =
        format!("modal-suppression-interaction-{}", presentation.value()).into();
    let state_focus = focus.clone();
    let state = window.use_keyed_state(state_id, cx, move |window, cx| {
        ModalSuppressionState::new(state_focus, window, cx)
    });
    press_owner.register(
        &state,
        ModalSuppressionState::cancel_modal_owned_press,
        |state| state.interaction.is_idle(),
    );
    state.update(cx, |state, cx| state.synchronize(enabled, cx));

    let down_state = state.clone();
    let move_state = state.clone();
    let up_state = state.clone();
    let exit_state = state.clone();
    let pointer_owner = owner.clone();
    let pointer_tracker = canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
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
                if phase.capture() {
                    move_state.update(cx, |state, cx| {
                        state.pointer_move(
                            move_hitbox.is_hovered(window),
                            event.pressed_button == Some(MouseButton::Left),
                            cx,
                        );
                    });
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if !phase.capture()
                    || event.button != MouseButton::Left
                    || !up_state.read(cx).interaction.is_pointer_owned()
                {
                    return;
                }
                let activate = up_state.update(cx, |state, cx| {
                    state.pointer_up(hitbox.is_hovered(window), cx)
                });
                if activate {
                    toggle_alert_suppression(&pointer_owner, presentation, cx);
                }
                window.prevent_default();
                cx.stop_propagation();
            });
            window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                if phase.capture() {
                    exit_state.update(cx, |state, cx| state.cancel_pointer(cx));
                }
            });
        },
    )
    .absolute()
    .inset_0();

    let pressed = matches!(
        state.read(cx).interaction,
        ModalSuppressionInteraction::Space | ModalSuppressionInteraction::Pointer { inside: true }
    );
    let toggle_theme = *ControlHost::Floating.toggle_theme(cx);
    let hover = crate::HoverFade::new(
        ("modal-suppression-hover", presentation.value()),
        window,
        cx,
    );
    let toggle_paint = if enabled && !pressed {
        toggle_theme.paint(selected, enabled, false, false).mix(
            toggle_theme.paint(selected, enabled, true, false),
            hover.level(window, cx),
        )
    } else {
        toggle_theme.paint(selected, enabled, false, pressed)
    };
    let pressed_label = toggle_theme.paint(selected, enabled, false, true).label();
    let font = crate::control_typography(cx).regular().clone();
    let pressed_font = font.clone();
    let key_down_state = state.clone();
    let key_up_state = state;
    let keyboard_owner = owner;
    let keyboard_focus = focus.clone();
    let focus_anchor = focus_anchors.register(&focus);
    let scroll_anchor = focus_anchor.scroll_anchor();
    let control = div()
        .id(("modal-suppression", presentation.value()))
        .debug_selector(|| "modal-alert-suppression".to_owned())
        .relative()
        .group(crate::toggle::INTERACTION_GROUP)
        .max_w(relative(1.0))
        .min_w_0()
        .track_focus(&focus)
        .anchor_scroll(Some(scroll_anchor))
        .flex()
        .items_center()
        .gap(metrics.action_gap)
        .px(metrics.action_gap)
        .py(metrics.action_gap / 2.0)
        // The suppression row is part of the modal surface, not a panel resting on it: it carries
        // no fill or edge of its own, only the hit area and focus ring its checkbox needs.
        .rounded(metrics.control_radius)
        .text_size(metrics.body_size)
        .cursor_default()
        .block_mouse_except_scroll()
        .on_key_down(move |event: &KeyDownEvent, window, cx| {
            if event.keystroke.key != "space"
                || event.keystroke.modifiers.modified()
                || event.is_held
            {
                return;
            }
            window.prevent_default();
            key_down_state.update(cx, |state, cx| state.space_down(cx));
            cx.stop_propagation();
        })
        .on_key_up(move |event: &KeyUpEvent, window, cx| {
            if event.keystroke.key != "space" || !key_up_state.read(cx).interaction.is_space_owned()
            {
                return;
            }
            let may_activate =
                !event.keystroke.modifiers.modified() && keyboard_focus.is_focused(window);
            let activate = key_up_state.update(cx, |state, cx| state.space_up(may_activate, cx));
            if activate {
                toggle_alert_suppression(&keyboard_owner, presentation, cx);
            }
            window.prevent_default();
            cx.stop_propagation();
        })
        .child(crate::toggle::modal_checkbox_indicator(
            toggle_theme,
            selected,
            toggle_paint,
            enabled,
            pressed,
            focused,
            "modal-alert-suppression-indicator".to_owned(),
            "modal-alert-suppression-keyboard-focus".to_owned(),
        ))
        .child(
            div()
                .id("modal-suppression-label")
                .debug_selector(|| "modal-alert-suppression-label".to_owned())
                .font(font)
                .text_size(metrics.body_size)
                .line_height(relative(1.2))
                .text_color(toggle_paint.label())
                .child(label)
                .when(enabled && !pressed, |label| {
                    label.group_active(crate::toggle::INTERACTION_GROUP, move |style| {
                        crate::refine_control_text(
                            style,
                            &pressed_font,
                            metrics.body_size,
                            1.2,
                            pressed_label,
                        )
                    })
                }),
        )
        .child(pointer_tracker)
        .child(hover.tracker())
        .child(focus_anchor.bounds_tracker(shell.hairline()));

    div().flex().min_w_0().child(control).into_any_element()
}

struct ModalSuppressionState {
    interaction: ModalSuppressionInteraction,
    enabled: bool,
}

impl ModalSuppressionState {
    fn new(focus: FocusHandle, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_blur(&focus, window, |state, _, cx| state.cancel_keyboard(cx))
            .detach();
        cx.observe_window_activation(window, |state, window, cx| {
            if !window.is_window_active() {
                state.cancel_modal_owned_press(cx);
            }
        })
        .detach();
        Self {
            interaction: ModalSuppressionInteraction::Idle,
            enabled: false,
        }
    }

    fn synchronize(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.enabled = enabled;
        if !enabled {
            self.cancel_modal_owned_press(cx);
        }
    }

    fn pointer_down(&mut self, cx: &mut Context<Self>) {
        if self.enabled && self.interaction.pointer_down() {
            cx.notify();
        }
    }

    fn pointer_move(&mut self, inside: bool, left_held: bool, cx: &mut Context<Self>) {
        if self.interaction.pointer_move(inside, left_held) {
            cx.notify();
        }
    }

    fn pointer_up(&mut self, inside: bool, cx: &mut Context<Self>) -> bool {
        let released_inside = self.interaction.pointer_up(inside);
        let activate = self.enabled && released_inside;
        cx.notify();
        activate
    }

    fn space_down(&mut self, cx: &mut Context<Self>) {
        if self.enabled && self.interaction.space_down() {
            cx.notify();
        }
    }

    fn space_up(&mut self, focused: bool, cx: &mut Context<Self>) -> bool {
        let released_owned_press = self.interaction.space_up();
        let activate = self.enabled && focused && released_owned_press;
        cx.notify();
        activate
    }

    fn cancel_pointer(&mut self, cx: &mut Context<Self>) {
        if self.interaction.cancel_pointer() {
            cx.notify();
        }
    }

    fn cancel_keyboard(&mut self, cx: &mut Context<Self>) {
        if self.interaction.cancel_keyboard() {
            cx.notify();
        }
    }

    fn cancel_modal_owned_press(&mut self, cx: &mut Context<Self>) {
        if self.interaction.cancel() {
            cx.notify();
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ModalSuppressionInteraction {
    #[default]
    Idle,
    Pointer {
        inside: bool,
    },
    Space,
}

impl ModalSuppressionInteraction {
    fn is_idle(self) -> bool {
        self == Self::Idle
    }

    fn is_pointer_owned(self) -> bool {
        matches!(self, Self::Pointer { .. })
    }

    fn is_space_owned(self) -> bool {
        self == Self::Space
    }

    fn pointer_down(&mut self) -> bool {
        if !self.is_idle() {
            return false;
        }
        *self = Self::Pointer { inside: true };
        true
    }

    fn pointer_move(&mut self, inside: bool, left_held: bool) -> bool {
        let Self::Pointer {
            inside: previous_inside,
        } = self
        else {
            return false;
        };
        if !left_held {
            *self = Self::Idle;
            return true;
        }
        if *previous_inside == inside {
            return false;
        }
        *previous_inside = inside;
        true
    }

    fn pointer_up(&mut self, inside: bool) -> bool {
        if !self.is_pointer_owned() {
            return false;
        }
        *self = Self::Idle;
        inside
    }

    fn space_down(&mut self) -> bool {
        if !self.is_idle() {
            return false;
        }
        *self = Self::Space;
        true
    }

    fn space_up(&mut self) -> bool {
        if !self.is_space_owned() {
            return false;
        }
        *self = Self::Idle;
        true
    }

    fn cancel_pointer(&mut self) -> bool {
        if !self.is_pointer_owned() {
            return false;
        }
        *self = Self::Idle;
        true
    }

    fn cancel_keyboard(&mut self) -> bool {
        if !self.is_space_owned() {
            return false;
        }
        *self = Self::Idle;
        true
    }

    fn cancel(&mut self) -> bool {
        if self.is_idle() {
            return false;
        }
        *self = Self::Idle;
        true
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one private shared action-area renderer"
)]
fn render_footer(
    snapshot: &ModalRenderSnapshot,
    owner: WeakEntity<ModalWindowOwner>,
    axis: ActionAxis,
    arrangement: ActionArrangement,
    policy: ModalDesktopPolicy,
    direction: TextDirection,
    action_focus: Vec<FocusHandle>,
    button_press_owner: ModalPressOwner,
    footer_scroll: ScrollHandle,
    footer_focus_anchors: ModalFocusAnchorRegistry,
    maximum_height: gpui::Pixels,
    metrics: ModalMetrics,
    shell: FloatingShell,
) -> AnyElement {
    let presentation = snapshot.presentation;
    let ActionArrangement {
        physical,
        traversal,
        help,
    } = arrangement;
    let decisions_are_reversed = axis == ActionAxis::Horizontal && physical != traversal;
    if decisions_are_reversed {
        debug_assert_eq!(
            physical,
            traversal.iter().rev().copied().collect::<Vec<_>>(),
            "horizontal physical action placement must mirror logical traversal"
        );
    } else {
        debug_assert_eq!(physical, traversal);
    }
    let mut decisions = div()
        .flex()
        .when(axis == ActionAxis::Horizontal, |row| {
            row.flex_row()
                .when(decisions_are_reversed, |row| row.flex_row_reverse())
                .items_center()
                .justify_end()
        })
        .when(axis == ActionAxis::Vertical, |row| row.flex_col())
        .gap(metrics.action_gap)
        .min_w_0();
    for index in traversal {
        let Some(action) = snapshot.actions.get(index) else {
            continue;
        };
        decisions = decisions.child(render_action(
            action,
            index,
            presentation,
            owner.clone(),
            action_focus.get(index).cloned(),
            button_press_owner.clone(),
            axis == ActionAxis::Vertical,
            policy.default_action_presentation(action),
        ));
    }
    let has_help = !help.is_empty();
    let mut help_actions = div()
        .flex()
        .when(axis == ActionAxis::Horizontal, |actions| {
            actions
                .flex_row()
                .when(direction == TextDirection::RightToLeft, |actions| {
                    actions.flex_row_reverse()
                })
                .items_center()
        })
        .when(axis == ActionAxis::Vertical, |actions| actions.flex_col())
        .gap(metrics.action_gap)
        .min_w_0();
    for index in help {
        let Some(action) = snapshot.actions.get(index) else {
            continue;
        };
        help_actions = help_actions.child(render_action(
            action,
            index,
            presentation,
            owner.clone(),
            action_focus.get(index).cloned(),
            button_press_owner.clone(),
            axis == ActionAxis::Vertical,
            policy.default_action_presentation(action),
        ));
    }

    let footer = div()
        .id(("modal-footer", snapshot.presentation.value()))
        .debug_selector(move || format!("modal-footer-{}", presentation.value()))
        .flex_shrink_0()
        .min_h_0()
        .max_h(maximum_height)
        .overflow_x_hidden()
        .overflow_y_scroll()
        .track_scroll(&footer_scroll)
        .px(metrics.surface_padding)
        .py(metrics.section_gap)
        .border_t(shell.hairline())
        .border_color(shell.divider())
        .flex()
        .when(axis == ActionAxis::Horizontal, |footer| {
            footer
                .flex_row()
                .when(direction == TextDirection::RightToLeft, |footer| {
                    footer.flex_row_reverse()
                })
                .items_center()
                .when(has_help, |footer| footer.justify_between())
        })
        .when(axis == ActionAxis::Vertical, |footer| footer.flex_col())
        .gap(metrics.action_gap)
        .when(axis == ActionAxis::Horizontal && !has_help, |footer| {
            footer.child(div().flex_grow(1.0))
        })
        .when(has_help, |footer| footer.child(help_actions))
        .child(decisions)
        .into_any_element();
    ModalControlScopeElement {
        content: footer,
        controls: ModalControlScope::new(button_press_owner)
            .with_focus_anchors(footer_focus_anchors),
    }
    .into_any_element()
}

#[expect(
    clippy::too_many_arguments,
    reason = "one private action renderer consumes resolved semantics, policy presentation, and ownership"
)]
fn render_action(
    action: &ModalRenderAction,
    index: usize,
    presentation: ModalPresentationId,
    owner: WeakEntity<ModalWindowOwner>,
    focus: Option<FocusHandle>,
    button_press_owner: ModalPressOwner,
    full_width: bool,
    default_presentation: DefaultActionPresentation,
) -> AnyElement {
    let source_owner = owner;
    let variant = if action.intent == ModalActionIntent::Destructive {
        ButtonVariant::Destructive
    } else if default_presentation == DefaultActionPresentation::Emphasized
        || action.emphasis == ModalActionEmphasis::Prominent
    {
        ButtonVariant::Primary
    } else if action.role == ModalActionRole::Help {
        ButtonVariant::Link
    } else {
        ButtonVariant::Secondary
    };
    let role = match (action.role, action.intent) {
        (_, ModalActionIntent::Destructive) => ButtonRole::Destructive,
        (ModalActionRole::Cancel, _) => ButtonRole::Cancel,
        _ => ButtonRole::Normal,
    };
    let id = (
        ElementId::from(("modal-action", presentation.value())),
        action.debug_identity.clone(),
    );
    let selector = format!("modal-action-{}", action.debug_identity);
    let mut button = Button::new(id, action.label.clone())
        .size(ButtonSize::Small)
        .variant(variant)
        .role(role)
        .full_width(full_width)
        .multiline(full_width)
        .disabled(!action.enabled)
        .modal_borderless()
        .modal_press_owner(button_press_owner)
        .debug_selector(selector)
        .on_activate(move |activation, _, cx| {
            let source = match activation.source() {
                crate::ButtonActivationSource::Pointer => ModalActivationSource::Pointer,
                crate::ButtonActivationSource::Space => ModalActivationSource::Space,
                crate::ButtonActivationSource::Return => ModalActivationSource::Return,
            };
            request_action_from_renderer(&source_owner, presentation, index, source, cx);
        });
    if let Some(focus) = focus {
        button = button.modal_focus_handle(focus);
    }
    let button = button.into_any_element();
    match default_presentation {
        DefaultActionPresentation::None => button,
        DefaultActionPresentation::Emphasized => {
            let selector = format!("modal-action-default-emphasis-{}", action.debug_identity);
            div()
                .debug_selector(move || selector.clone())
                .relative()
                .when(full_width, |emphasis| emphasis.w_full())
                .child(button)
                .into_any_element()
        }
    }
}

fn enabled_action(index: Option<usize>, actions: &[ModalRenderAction]) -> Option<usize> {
    index.filter(|index| actions.get(*index).is_some_and(|action| action.enabled))
}

fn safe_cancel_action(index: Option<usize>, actions: &[ModalRenderAction]) -> Option<usize> {
    index.filter(|index| {
        actions
            .get(*index)
            .is_some_and(|action| is_safe_cancel(action.role, action.intent, action.enabled))
    })
}

struct ModalFocusRing {
    scope: FocusHandle,
    surface: FocusHandle,
    leading: FocusHandle,
    trailing: FocusHandle,
    suppression: FocusHandle,
    action_focus: Vec<FocusHandle>,
    body_scroll: ScrollHandle,
    body_scrollbar: Entity<OverlayScrollbar<f32>>,
    footer_scroll: ScrollHandle,
    body_focus_anchors: ModalFocusAnchorRegistry,
    footer_focus_anchors: ModalFocusAnchorRegistry,
    presentation: Option<ModalPresentationId>,
    initial: PreparedFocusIntent,
    focus_request_generation: u64,
    initialized: bool,
    owned_focus_before_render: bool,
    pending_reveal: Option<gpui::WeakFocusHandle>,
}

impl ModalFocusRing {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scope = cx.focus_handle();
        let surface = cx.focus_handle();
        let leading = cx.focus_handle().tab_stop(true);
        let trailing = cx.focus_handle().tab_stop(true);
        let suppression = cx.focus_handle();
        let body_scroll = ScrollHandle::new();
        let body_scrollbar = cx.new(|_| OverlayScrollbar::<f32>::new("modal-body-scrollbar"));
        cx.subscribe_in(
            &body_scrollbar,
            window,
            |state, _, event: &OverlayScrollbarEvent<f32>, window, _| {
                if let OverlayScrollbarEvent::OffsetRequested(offset) = event {
                    let current = state.body_scroll.offset();
                    state
                        .body_scroll
                        .set_offset(gpui::point(current.x, px(-*offset)));
                    window.refresh();
                }
            },
        )
        .detach();
        let footer_scroll = ScrollHandle::new();
        let body_focus_anchors = ModalFocusAnchorRegistry::new(body_scroll.clone());
        let footer_focus_anchors = ModalFocusAnchorRegistry::new(footer_scroll.clone());
        cx.on_focus(&leading, window, |state, window, cx| {
            state.focus_last(window, cx)
        })
        .detach();
        cx.on_focus(&trailing, window, |state, window, cx| {
            state.focus_first(window, cx)
        })
        .detach();
        cx.on_focus_out(&scope, window, |state, _, window, cx| {
            state.repair_focus_loss(window, cx)
        })
        .detach();
        Self {
            scope,
            surface,
            leading,
            trailing,
            suppression,
            action_focus: Vec::new(),
            body_scroll,
            body_scrollbar,
            footer_scroll,
            body_focus_anchors,
            footer_focus_anchors,
            presentation: None,
            initial: PreparedFocusIntent::Surface,
            focus_request_generation: 0,
            initialized: false,
            owned_focus_before_render: false,
            pending_reveal: None,
        }
    }

    fn synchronize(
        &mut self,
        actions: &[ModalRenderAction],
        initial: &PreparedFocusIntent,
        focus_request_generation: u64,
        suppression_available: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.body_focus_anchors.reset();
        self.footer_focus_anchors.reset();
        while self.action_focus.len() < actions.len() {
            self.action_focus.push(cx.focus_handle());
        }
        for (focus, action) in self.action_focus.iter_mut().zip(actions) {
            *focus = focus.clone().tab_stop(action.enabled);
        }
        self.suppression = self.suppression.clone().tab_stop(suppression_available);
        if self.initial != *initial || self.focus_request_generation != focus_request_generation {
            self.initial = initial.clone();
            self.focus_request_generation = focus_request_generation;
            self.initialized = false;
        }
        self.owned_focus_before_render = self.scope.contains_focused(window, cx);
    }

    fn apply_focus_reveal(&self, focus: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        if !self.body_focus_anchors.reveal(focus, window, cx) {
            self.footer_focus_anchors.reveal(focus, window, cx);
        }
    }

    fn reveal_focus(&mut self, focus: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_focus_reveal(focus, window, cx);
        self.pending_reveal = Some(focus.downgrade());
    }

    fn has_focused_action(&self, window: &Window) -> bool {
        self.action_focus
            .iter()
            .any(|focus| focus.is_focused(window))
    }

    fn reveal_current_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(focused) = window.focused(cx) {
            self.reveal_focus(&focused, window, cx);
        }
    }

    fn focus_first(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leading.focus(window, cx);
        window.focus_next(cx);
        if self.trailing.is_focused(window) {
            self.surface.focus(window, cx);
        }
        self.reveal_current_focus(window, cx);
    }

    fn focus_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_first(window, cx);
        } else {
            window.focus_next(cx);
            self.reveal_current_focus(window, cx);
        }
    }

    fn focus_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_last(window, cx);
        } else {
            window.focus_prev(cx);
            self.reveal_current_focus(window, cx);
        }
    }

    fn focus_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.trailing.focus(window, cx);
        window.focus_prev(cx);
        if self.leading.is_focused(window) {
            self.surface.focus(window, cx);
        }
        self.reveal_current_focus(window, cx);
    }

    fn repair_focus_loss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() || super::window_has_owned_popup(window, cx) {
            return;
        }
        let Some(presentation) = self.presentation else {
            return;
        };
        let expected = super::ModalParentToken {
            window_id: window.window_handle().window_id(),
            presentation,
        };
        if super::current_modal_parent(window, cx) == Some(expected) {
            self.focus_first(window, cx);
        }
    }

    fn reconcile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if super::window_has_owned_popup(window, cx) {
            return;
        }
        if let Some(focus) = self.pending_reveal.take().and_then(|focus| focus.upgrade())
            && focus.is_focused(window)
        {
            self.apply_focus_reveal(&focus, window, cx);
        }
        if !self.initialized {
            let requested = match &self.initial {
                PreparedFocusIntent::Action(index) => self.action_focus.get(*index).cloned(),
                PreparedFocusIntent::Body(body) => Some(body.clone()),
                PreparedFocusIntent::Surface => Some(self.surface.clone()),
            };
            if let Some(requested) = requested
                && self.scope.contains(&requested, window)
            {
                requested.focus(window, cx);
                if !matches!(self.initial, PreparedFocusIntent::Surface)
                    && window.focused(cx).is_some_and(|focused| !focused.tab_stop)
                {
                    self.focus_first(window, cx);
                } else {
                    self.reveal_focus(&requested, window, cx);
                }
            } else {
                self.focus_first(window, cx);
            }
            self.initialized = true;
        } else {
            let focused_inside = self.scope.contains_focused(window, cx);
            let focused_tab_stop_is_invalid = focused_inside
                && window
                    .focused(cx)
                    .is_some_and(|focused| !focused.tab_stop && !self.surface.is_focused(window));
            if focused_tab_stop_is_invalid || (self.owned_focus_before_render && !focused_inside) {
                self.focus_first(window, cx);
            }
        }
    }
}

fn schedule_focus_reconciliation(
    state: gpui::Entity<ModalFocusRing>,
    window: &Window,
    cx: &mut App,
) {
    window.defer(cx, move |window, cx| {
        state.update(cx, |state, cx| state.reconcile(window, cx));
    });
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
