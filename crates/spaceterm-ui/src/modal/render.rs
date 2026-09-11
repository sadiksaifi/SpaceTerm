use gpui::{
    AnyElement, App, Bounds, Context, Element, ElementId, FocusHandle, GlobalElementId,
    HitboxBehavior, ImageSource, InspectorElementId, InteractiveElement as _, IntoElement,
    KeyBinding, KeyDownEvent, KeyUpEvent, LayoutId, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, RenderOnce, Rgba, ScrollHandle,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity,
    Window, actions, canvas, div, img, prelude::FluentBuilder as _, px, relative, size,
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
    Button, ButtonRole, ButtonSize, ButtonVariant, Icon, IconName,
    button::{
        ModalControlScope, ModalFocusAnchorRegistry, ModalPressOwner,
        measure_button_intrinsic_width,
    },
};

const MODAL_KEY_CONTEXT: &str = "SpaceTermModal";
const INDETERMINATE_SEGMENT_COUNT: usize = 4;

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
/// Place it around the complete root content, normally as
/// `ModalLayer::new(TooltipLayer::new(content))`. The active modal is painted as the final normal
/// child rather than a deferred draw, allowing a modal-owned deferred Menu to remain above it. The
/// full-viewport scrim blocks outside pointer press, release, move, and wheel input without outside
/// dismissal or click-through. The modal key context blocks underlay keyboard routing while the
/// leading and trailing sentinels contain the complete current-frame GPUI tab-stop order.
///
/// GPUI 0.2.2 does not let this custom layer exclude the underlay from the native accessibility
/// tree. Private logical semantic snapshots and debug selectors test retained facts and observable
/// modality, but are not native accessibility evidence.
#[derive(IntoElement)]
pub struct ModalLayer {
    content: AnyElement,
}

impl ModalLayer {
    /// Wraps complete Operating-System Window content.
    pub fn new(content: impl IntoElement) -> Self {
        Self {
            content: content.into_any_element(),
        }
    }
}

impl RenderOnce for ModalLayer {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let root = window.use_keyed_state("spaceterm-modal-root-scope", cx, ModalRootScope::new);
        let (root_focus, owner) =
            root.read_with(cx, |root, _| (root.focus.clone(), root.owner.clone()));
        register_root_scope(&owner, &root_focus, cx);

        div()
            .id("spaceterm-modal-root")
            .debug_selector(|| "spaceterm-modal-root".to_owned())
            .relative()
            .size_full()
            .track_focus(&root_focus)
            .child(self.content)
            .child(ModalOwnerView { owner })
    }
}

struct ModalRootScope {
    focus: FocusHandle,
    owner: gpui::Entity<ModalWindowOwner>,
}

impl ModalRootScope {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let owner = modal_owner_for_layer(window, cx);
        let press_owner = owner.read_with(cx, |owner, _| owner.press_owner());
        cx.observe_window_activation(window, move |_, window, cx| {
            if !window.is_window_active() {
                press_owner.disarm(cx);
            }
        })
        .detach();
        cx.on_release(|state, cx| retire_window_owner(&state.owner, cx))
            .detach();
        Self {
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
    let theme = *cx.global::<ModalTheme>();
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
    let typography = crate::control_typography(cx);
    let viewport = window.viewport_size();
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
    let geometry = clamp_surface_to_viewport(viewport, size(desired_width, height_cap), metrics);
    let available_actions = (geometry.size.width - metrics.surface_padding * 2.0).max(px(1.0));
    let measured_widths = snapshot
        .actions
        .iter()
        .map(|action| measure_button_intrinsic_width(&action.label, ButtonSize::Small, window, cx))
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
        body_focus_anchors,
        metrics,
        paint,
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
        paint,
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
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(metrics.corner_radius)
        .border(metrics.border_width)
        .border_color(paint.border)
        .bg(paint.surface)
        .text_color(paint.primary_text)
        .font(typography.regular().clone())
        .track_focus(&scope)
        .key_context(MODAL_KEY_CONTEXT)
        .on_action(move |_: &TraverseForward, window, cx| {
            if !crate::menu::window_menu_is_owned_by_current_modal(window, cx) {
                forward_focus.update(cx, |state, cx| state.focus_next(window, cx));
            }
            cx.stop_propagation();
        })
        .on_action(move |_: &TraverseBackward, window, cx| {
            if !crate::menu::window_menu_is_owned_by_current_modal(window, cx) {
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
            if crate::menu::window_menu_is_owned_by_current_modal(window, cx) {
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
        .child(footer)
        .child(div().size_0().track_focus(&trailing));

    div()
        .id(("modal-overlay", presentation.value()))
        .absolute()
        .inset_0()
        .child(div().absolute().inset_0().bg(paint.scrim))
        .child(blocker)
        .child(surface)
        .into_any_element()
}

fn render_blocker(geometry: ModalSurfaceGeometry, press_owner: ModalPressOwner) -> AnyElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::BlockMouse),
        move |_, _, window, _| {
            let down_owner = press_owner.clone();
            let up_owner = press_owner.clone();
            let move_owner = press_owner.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase.capture() && !surface_contains(geometry, event.position) {
                    down_owner.disarm(cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase.capture() && !surface_contains(geometry, event.position) {
                    up_owner.disarm(cx);
                    window.prevent_default();
                    cx.stop_propagation();
                }
            });
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase.capture() && !surface_contains(geometry, event.position) {
                    move_owner.disarm(cx);
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
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
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
    background: Rgba,
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
            background: paint.informational_background,
        },
        super::AlertIntent::Warning => AlertIntentPresentation {
            icon: IconName::TriangleAlert,
            selector: "warning",
            accent: paint.warning,
            background: paint.warning_background,
        },
        super::AlertIntent::Critical => AlertIntentPresentation {
            icon: IconName::OctagonAlert,
            selector: "critical",
            accent: paint.critical,
            background: paint.critical_background,
        },
    }
}

fn render_header(
    snapshot: &ModalRenderSnapshot,
    maximum_height: gpui::Pixels,
    metrics: ModalMetrics,
    paint: ModalPaint,
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
        .border_b(metrics.border_width)
        .border_color(paint.divider)
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
    body_focus_anchors: ModalFocusAnchorRegistry,
    metrics: ModalMetrics,
    paint: ModalPaint,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
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
                .gap(metrics.action_gap)
                .child(
                    div()
                        .debug_selector(move || marker_selector.clone())
                        .size(marker_extent)
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(marker_extent / 2.0)
                        .border(metrics.border_width)
                        .border_color(intent_presentation.accent)
                        .bg(intent_presentation.background)
                        .child(Icon::new(
                            intent_presentation.icon,
                            marker_extent * 0.55,
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
                        paint,
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
                        .child(status),
                )
                .child(render_progress(progress_state, metrics, paint))
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

    div()
        .id(("modal-body", snapshot.presentation.value()))
        .debug_selector(|| "modal-body-viewport".to_owned())
        .flex_1()
        .min_h_0()
        .min_w_0()
        .overflow_x_hidden()
        .overflow_y_scroll()
        .track_scroll(&body_scroll)
        .child(
            div()
                .w_full()
                .min_w_0()
                .p(metrics.surface_padding)
                .child(content),
        )
        .into_any_element()
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
    paint: ModalPaint,
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

    let key_down_state = state.clone();
    let key_up_state = state;
    let keyboard_owner = owner;
    let keyboard_focus = focus.clone();
    let focus_anchor = focus_anchors.register(&focus);
    let scroll_anchor = focus_anchor.scroll_anchor();
    let checkbox_icon = if selected {
        IconName::SquareCheckBig
    } else {
        IconName::Square
    };
    let checkbox_color = if !enabled {
        paint.suppression_disabled
    } else if focused {
        paint.suppression_focused
    } else if selected {
        paint.suppression_selected
    } else {
        paint.suppression_unselected
    };
    let control = div()
        .id(("modal-suppression", presentation.value()))
        .debug_selector(|| "modal-alert-suppression".to_owned())
        .relative()
        .max_w(relative(1.0))
        .min_w_0()
        .track_focus(&focus)
        .anchor_scroll(Some(scroll_anchor))
        .flex()
        .items_center()
        .gap(metrics.action_gap)
        .px(metrics.action_gap)
        .py(metrics.action_gap / 2.0)
        .rounded(metrics.corner_radius)
        .border(metrics.border_width)
        .border_color(paint.surface)
        .bg(paint.surface)
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
        .child(Icon::new(checkbox_icon, metrics.body_size, checkbox_color))
        .child(label)
        .when(focused, |control| {
            control.child(
                div()
                    .debug_selector(|| "modal-alert-suppression-keyboard-focus".to_owned())
                    .absolute()
                    .top(-metrics.border_width * 3.0)
                    .right(-metrics.border_width * 3.0)
                    .bottom(-metrics.border_width * 3.0)
                    .left(-metrics.border_width * 3.0)
                    .rounded(metrics.corner_radius + metrics.border_width * 2.0)
                    .border(metrics.border_width)
                    .border_color(paint.progress_fill),
            )
        })
        .child(pointer_tracker)
        .child(focus_anchor.bounds_tracker(metrics.border_width));

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

fn render_progress(
    state: Option<ProgressState>,
    metrics: ModalMetrics,
    paint: ModalPaint,
) -> AnyElement {
    let fill = match state.unwrap_or(ProgressState::Indeterminate) {
        ProgressState::Determinate(value) => div()
            .debug_selector(|| "modal-progress-determinate".to_owned())
            .h_full()
            .w(relative(value.value()))
            .bg(paint.progress_fill),
        ProgressState::Indeterminate => {
            let mut segments = div()
                .debug_selector(|| "modal-progress-indeterminate".to_owned())
                .h_full()
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .justify_between();
            for index in 0..INDETERMINATE_SEGMENT_COUNT {
                segments = segments.child(
                    div()
                        .debug_selector(move || {
                            format!("modal-progress-indeterminate-segment-{index}")
                        })
                        .h_full()
                        .w(relative(metrics.indeterminate_segment_fraction()))
                        .bg(paint.progress_fill),
                );
            }
            segments
        }
    };
    div()
        .debug_selector(|| "modal-progress-track".to_owned())
        .w_full()
        .h(metrics.progress_track_thickness())
        .flex_shrink_0()
        .overflow_hidden()
        .rounded(metrics.progress_track_radius())
        .bg(paint.progress_track)
        .child(fill)
        .into_any_element()
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
    paint: ModalPaint,
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
        .border_t(metrics.border_width)
        .border_color(paint.divider)
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
            footer.child(div().flex_grow())
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
        self.leading.focus(window);
        window.focus_next();
        if self.trailing.is_focused(window) {
            self.surface.focus(window);
        }
        self.reveal_current_focus(window, cx);
    }

    fn focus_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_first(window, cx);
        } else {
            window.focus_next();
            self.reveal_current_focus(window, cx);
        }
    }

    fn focus_previous(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_last(window, cx);
        } else {
            window.focus_prev();
            self.reveal_current_focus(window, cx);
        }
    }

    fn focus_last(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.trailing.focus(window);
        window.focus_prev();
        if self.leading.is_focused(window) {
            self.surface.focus(window);
        }
        self.reveal_current_focus(window, cx);
    }

    fn repair_focus_loss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active()
            || crate::menu::window_menu_is_owned_by_current_modal(window, cx)
        {
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
        if crate::menu::window_menu_is_owned_by_current_modal(window, cx) {
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
                requested.focus(window);
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
