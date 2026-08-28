use gpui::{
    AnyElement, App, Bounds, Context, Element, ElementId, FocusHandle, GlobalElementId,
    HitboxBehavior, ImageSource, InspectorElementId, InteractiveElement as _, IntoElement,
    KeyBinding, KeyDownEvent, KeyUpEvent, LayoutId, MouseButton, MouseDownEvent, MouseExitEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, RenderOnce, Rgba, ScrollWheelEvent,
    SharedString, StatefulInteractiveElement as _, Styled as _, WeakEntity, Window, actions,
    canvas, div, img, prelude::FluentBuilder as _, px, relative, size,
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
    Button, ButtonRole, ButtonSize, ButtonVariant,
    button::{ModalControlScope, ModalPressOwner, measure_button_intrinsic_width},
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
/// Generic control initialization already installs portable Tab, Shift-Tab, Return, and Escape
/// behavior. Applications call this separately to opt into desktop-specific equivalents without
/// requiring host-platform detection in the reusable library.
pub fn install_modal_keybindings(cx: &mut App, profile: ModalKeybindingProfile) {
    match profile {
        ModalKeybindingProfile::MacOs => cx.bind_keys([KeyBinding::new(
            "cmd-.",
            ActivatePlatformCancel,
            Some(MODAL_KEY_CONTEXT),
        )]),
    }
}

pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", TraverseForward, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("shift-tab", TraverseBackward, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("enter", ActivateDefault, Some(MODAL_KEY_CONTEXT)),
        KeyBinding::new("escape", ActivateCancel, Some(MODAL_KEY_CONTEXT)),
    ]);
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
    let viewport = window.viewport_size();
    let desired_width = metrics.width_for(match snapshot.kind {
        ModalKind::Alert | ModalKind::Progress => DialogSize::Compact,
        ModalKind::Dialog => snapshot.dialog_size,
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
    let axis = select_action_axis(available_actions, &measured_widths, metrics);
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
    let (scope, surface_focus, leading, trailing, suppression_focus, action_focus) = {
        let state = focus_state.read(cx);
        (
            state.scope.clone(),
            state.surface.clone(),
            state.leading.clone(),
            state.trailing.clone(),
            state.suppression.clone(),
            state.action_focus.clone(),
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
    );
    let suppression_is_focused = suppression_focus.is_focused(window);
    let body = render_body(
        &snapshot,
        owner.clone(),
        suppression_focus,
        suppression_is_focused,
        press_owner.clone(),
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
        geometry.size.height * metrics.footer_maximum_fraction(),
        metrics,
        paint,
    );
    let presentation = snapshot.presentation;
    let default_action = enabled_action(snapshot.default_action, &snapshot.actions);
    let cancel_action = safe_cancel_action(snapshot.cancel_action, &snapshot.actions);
    let forward_focus = focus_state.clone();
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

#[derive(Clone, Copy, Debug, PartialEq)]
struct AlertIntentPresentation {
    marker: &'static str,
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
            marker: "i",
            selector: "informational",
            accent: paint.informational,
            background: paint.informational_background,
        },
        super::AlertIntent::Warning => AlertIntentPresentation {
            marker: "!",
            selector: "warning",
            accent: paint.warning,
            background: paint.warning_background,
        },
        super::AlertIntent::Critical => AlertIntentPresentation {
            marker: "×",
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
                .font_weight(gpui::FontWeight::SEMIBOLD)
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
                    AlertAccessory::Icon { image: None, .. } => div()
                        .size(extent)
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(metrics.title_size)
                        .text_color(paint.secondary_text)
                        .child("◇")
                        .into_any_element(),
                }
            });
            let intent_presentation = alert_intent_presentation(*intent, paint);
            let intent_selector = format!("modal-alert-intent-{}", intent_presentation.selector);
            let marker_selector =
                format!("modal-alert-intent-mark-{}", intent_presentation.selector);
            let message_panel = div()
                .debug_selector(move || intent_selector.clone())
                .flex()
                .min_w_0()
                .overflow_hidden()
                .rounded(metrics.corner_radius)
                .border(metrics.border_width)
                .border_color(intent_presentation.accent)
                .bg(intent_presentation.background)
                .child(
                    div()
                        .debug_selector(move || marker_selector.clone())
                        .w(metrics.accessory_extent() / 2.0)
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .py(metrics.action_gap)
                        .text_size(metrics.title_size)
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(intent_presentation.accent)
                        .child(intent_presentation.marker),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .p(metrics.action_gap)
                        .child(
                            div()
                                .debug_selector(|| "modal-alert-message".to_owned())
                                .text_size(metrics.body_size)
                                .whitespace_normal()
                                .child(message.clone()),
                        )
                        .when_some(detail.clone(), |content, detail| {
                            content.child(
                                div()
                                    .mt(metrics.action_gap)
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
                        metrics,
                        paint,
                        window,
                        cx,
                    ))
                })
                .into_any_element()
        }
        PreparedModalSemantics::Dialog { .. } => ModalBodyControlScope {
            content: snapshot
                .body
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
            controls: ModalControlScope::new(press_owner.clone()),
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
        .child(
            div()
                .w_full()
                .min_w_0()
                .p(metrics.surface_padding)
                .child(content),
        )
        .into_any_element()
}

struct ModalBodyControlScope {
    content: AnyElement,
    controls: ModalControlScope,
}

impl IntoElement for ModalBodyControlScope {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for ModalBodyControlScope {
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
    div()
        .id(("modal-suppression", presentation.value()))
        .debug_selector(|| "modal-alert-suppression".to_owned())
        .relative()
        .track_focus(&focus)
        .flex()
        .items_center()
        .gap(metrics.action_gap)
        .px(metrics.action_gap)
        .py(metrics.action_gap / 2.0)
        .rounded(metrics.corner_radius)
        .border(metrics.border_width)
        .border_color(if focused {
            paint.progress_fill
        } else {
            paint.border
        })
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
        .child(if selected { "☑" } else { "☐" })
        .child(label)
        .when(focused, |control| {
            control.child(
                div()
                    .debug_selector(|| "modal-alert-suppression-keyboard-focus".to_owned())
                    .absolute()
                    .inset(metrics.border_width * 2.0)
                    .rounded((metrics.corner_radius - metrics.border_width * 2.0).max(px(0.0)))
                    .border(metrics.border_width)
                    .border_color(paint.progress_fill),
            )
        })
        .child(pointer_tracker)
        .into_any_element()
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
            metrics,
            paint,
        ));
    }
    let has_help = !help.is_empty();
    let mut help_actions = div()
        .flex()
        .when(axis == ActionAxis::Horizontal, |actions| {
            actions.flex_row().items_center()
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
            metrics,
            paint,
        ));
    }

    div()
        .id(("modal-footer", snapshot.presentation.value()))
        .debug_selector(move || format!("modal-footer-{}", presentation.value()))
        .flex_shrink_0()
        .min_h_0()
        .max_h(maximum_height)
        .overflow_x_hidden()
        .overflow_y_scroll()
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
    metrics: ModalMetrics,
    paint: ModalPaint,
) -> AnyElement {
    let source_owner = owner;
    let variant = if action.intent == ModalActionIntent::Destructive {
        ButtonVariant::Destructive
    } else if action.emphasis == ModalActionEmphasis::Prominent {
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
        .modal_press_owner(button_press_owner)
        .debug_selector(selector)
        .on_activate(move |activation, _, cx| {
            let source = match activation.source() {
                crate::ButtonActivationSource::Pointer => ModalActivationSource::Pointer,
                crate::ButtonActivationSource::Keyboard => ModalActivationSource::Space,
            };
            request_action_from_renderer(&source_owner, presentation, index, source, cx);
        });
    if let Some(focus) = focus {
        button = button.modal_focus_handle(focus);
    }
    let button = button.into_any_element();
    match default_presentation {
        DefaultActionPresentation::None => button,
        DefaultActionPresentation::Ring => {
            let selector = format!("modal-action-default-ring-{}", action.debug_identity);
            div()
                .debug_selector(move || selector.clone())
                .relative()
                .when(full_width, |ring| ring.w_full())
                .child(button)
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .rounded(metrics.corner_radius)
                        .border(metrics.border_width)
                        .border_color(paint.default_ring),
                )
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
    presentation: Option<ModalPresentationId>,
    initial: PreparedFocusIntent,
    focus_request_generation: u64,
    initialized: bool,
    owned_focus_before_render: bool,
}

impl ModalFocusRing {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let scope = cx.focus_handle();
        let surface = cx.focus_handle();
        let leading = cx.focus_handle().tab_stop(true);
        let trailing = cx.focus_handle().tab_stop(true);
        let suppression = cx.focus_handle();
        cx.on_focus(&leading, window, |state, window, _| {
            state.focus_last(window)
        })
        .detach();
        cx.on_focus(&trailing, window, |state, window, _| {
            state.focus_first(window)
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
            presentation: None,
            initial: PreparedFocusIntent::Surface,
            focus_request_generation: 0,
            initialized: false,
            owned_focus_before_render: false,
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

    fn focus_first(&self, window: &mut Window) {
        self.leading.focus(window);
        window.focus_next();
        if self.trailing.is_focused(window) {
            self.surface.focus(window);
        }
    }

    fn focus_next(&self, window: &mut Window, cx: &App) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_first(window);
        } else {
            window.focus_next();
        }
    }

    fn focus_previous(&self, window: &mut Window, cx: &App) {
        if self.surface.is_focused(window) || !self.scope.contains_focused(window, cx) {
            self.focus_last(window);
        } else {
            window.focus_prev();
        }
    }

    fn focus_last(&self, window: &mut Window) {
        self.trailing.focus(window);
        window.focus_prev();
        if self.leading.is_focused(window) {
            self.surface.focus(window);
        }
    }

    fn repair_focus_loss(&self, window: &mut Window, cx: &App) {
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
            self.focus_first(window);
        }
    }

    fn reconcile(&mut self, window: &mut Window, cx: &App) {
        if crate::menu::window_menu_is_owned_by_current_modal(window, cx) {
            return;
        }
        if !self.initialized {
            let requested = match &self.initial {
                PreparedFocusIntent::Action(index) => self.action_focus.get(*index),
                PreparedFocusIntent::Body(body) => Some(body),
                PreparedFocusIntent::Surface => Some(&self.surface),
            };
            if let Some(requested) = requested
                && self.scope.contains(requested, window)
            {
                requested.focus(window);
                if !matches!(self.initial, PreparedFocusIntent::Surface)
                    && window.focused(cx).is_some_and(|focused| !focused.tab_stop)
                {
                    self.focus_first(window);
                }
            } else {
                self.focus_first(window);
            }
            self.initialized = true;
        } else {
            let focused_inside = self.scope.contains_focused(window, cx);
            let focused_tab_stop_is_invalid = focused_inside
                && window
                    .focused(cx)
                    .is_some_and(|focused| !focused.tab_stop && !self.surface.is_focused(window));
            if focused_tab_stop_is_invalid || (self.owned_focus_before_render && !focused_inside) {
                self.focus_first(window);
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
mod tests {
    use std::{
        cell::{Cell, RefCell},
        rc::Rc,
        time::Duration,
    };

    use gpui::{
        Action, AppContext as _, Bounds, Context, Entity, EntityInputHandler as _, KeyDownEvent,
        KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseMoveEvent, Render, ScrollDelta,
        TestAppContext, TouchPhase, VisualTestContext, WindowBounds, WindowHandle, WindowOptions,
        point, rgba,
    };

    use super::*;
    use crate::{
        Alert, AlertOutcome, AlertSuppression, ButtonMetrics, ButtonPaint, ButtonSizes,
        ButtonTheme, ButtonVariantStyle, ButtonVariants, DeterminateProgress, Dialog,
        DialogCloseDecision, DialogInitialFocus, DialogOutcome, DialogPendingCompletion,
        MAX_PROGRESS_STATUS_CHARACTERS, Menu, MenuEntry, MenuMetrics, MenuPaint, MenuSize,
        MenuSizes, MenuTheme, ModalAction, ModalActionIntent, ModalCloseReason,
        ModalDismissalError, ModalId, ModalLifecycleEvent, ModalTerminalOutcomeError,
        ModalUpdateError, ProgressCancelDecision, ProgressCancellation,
        ProgressCancellationCompletion, ProgressDialog, ProgressDialogOutcome,
        ProgressDialogUpdate, ProgressState, TextInput, TextInputEscapeBehavior,
        TextInputKeybindingProfile, TextInputMetrics, TextInputPaint, TextInputReturnBehavior,
        TextInputTheme, TextInputVariants, TooltipLayer, install_modal_keybindings,
        install_modal_policy, install_modal_theme, install_text_input_keybindings,
    };

    #[gpui::test]
    fn generic_modal_initialization_does_not_install_command_period(cx: &mut TestAppContext) {
        cx.update(super::init);
        let command_period = Keystroke::parse("cmd-.").expect("test key should parse");

        let has_platform_cancel = cx.update(|cx| {
            cx.all_bindings_for_input(&[command_period])
                .iter()
                .any(|binding| binding.action().name() == ActivatePlatformCancel.name())
        });

        assert!(!has_platform_cancel);
    }

    #[gpui::test]
    fn alternate_modal_policy_does_not_install_command_period(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::win_ui_for_tests()));
        let command_period = Keystroke::parse("cmd-.").expect("test key should parse");

        let has_platform_cancel = cx.update(|cx| {
            cx.all_bindings_for_input(&[command_period])
                .iter()
                .any(|binding| binding.action().name() == ActivatePlatformCancel.name())
        });

        assert!(!has_platform_cancel);
    }

    #[gpui::test]
    fn mac_os_modal_keybinding_profile_installs_command_period(cx: &mut TestAppContext) {
        cx.update(super::init);
        cx.update(|cx| install_modal_keybindings(cx, ModalKeybindingProfile::MacOs));
        let command_period = Keystroke::parse("cmd-.").expect("test key should parse");

        let has_platform_cancel = cx.update(|cx| {
            cx.all_bindings_for_input(&[command_period])
                .iter()
                .any(|binding| binding.action().name() == ActivatePlatformCancel.name())
        });

        assert!(has_platform_cancel);
    }

    fn test_button_theme() -> ButtonTheme {
        test_button_theme_scaled(1.0)
    }

    fn test_button_theme_scaled(factor: f32) -> ButtonTheme {
        test_button_theme_scaled_with_focus(factor, rgba(0x00aaffff))
    }

    fn test_button_theme_scaled_with_focus(factor: f32, focus_border: Rgba) -> ButtonTheme {
        let paint = ButtonPaint::new(rgba(0x303030ff), rgba(0xffffffff), rgba(0x606060ff));
        let style = ButtonVariantStyle::new(paint, paint, paint, paint);
        let metrics = ButtonMetrics::new(px(24.0 * factor))
            .horizontal_padding(px(8.0 * factor))
            .gap(px(6.0 * factor))
            .corner_radius(px(5.0 * factor))
            .font_size(px(12.0 * factor));
        ButtonTheme::new(
            ButtonVariants::new(style, style, style, style, style, style),
            ButtonSizes::new(metrics, metrics, metrics, metrics),
            focus_border,
        )
    }

    fn test_menu_theme() -> MenuTheme {
        let paint = MenuPaint::new(
            rgba(0x202024ff),
            rgba(0x606068ff),
            rgba(0xffffffff),
            rgba(0xb0b0b8ff),
            rgba(0x707078ff),
            rgba(0x404048ff),
            rgba(0xffffffff),
            rgba(0xff6677ff),
            rgba(0x505058ff),
        );
        let metrics = MenuMetrics::new(px(180.0), px(26.0));
        MenuTheme::new(paint, MenuSizes::new(metrics, metrics, metrics))
    }

    #[test]
    fn escape_and_command_period_target_rejects_destructive_cancel_render_state() {
        let actions = vec![ModalRenderAction {
            label: "Cancel".into(),
            role: ModalActionRole::Cancel,
            intent: ModalActionIntent::Destructive,
            emphasis: ModalActionEmphasis::Standard,
            enabled: true,
            is_default: false,
            debug_identity: "destructive-cancel".into(),
        }];

        assert_eq!(safe_cancel_action(Some(0), &actions), None);
    }

    #[test]
    fn suppression_interaction_rejects_repeats_and_mismatched_releases() {
        let mut interaction = ModalSuppressionInteraction::default();

        assert!(interaction.space_down());
        assert!(!interaction.space_down());
        assert!(!interaction.pointer_down());
        assert!(!interaction.pointer_up(true));
        assert!(interaction.space_up());
        assert!(!interaction.space_up());
        assert!(interaction.pointer_down());
        assert!(!interaction.space_up());
        assert!(interaction.pointer_up(true));
        assert!(interaction.is_idle());
    }

    fn test_modal_theme(metrics: ModalMetrics) -> ModalTheme {
        ModalTheme::new(
            ModalPaint::new(
                rgba(0x00000099),
                rgba(0x202024ff),
                rgba(0x606068ff),
                rgba(0xffffffff),
                rgba(0xb0b0b8ff),
                rgba(0x505058ff),
                rgba(0x404048ff),
                rgba(0x55aaffff),
                rgba(0x66aaffff),
                rgba(0x5599ffff),
                rgba(0x5599ff22),
                rgba(0xffbb55ff),
                rgba(0xffbb5522),
                rgba(0xff6677ff),
                rgba(0xff667722),
            ),
            metrics,
        )
    }

    fn test_modal_theme_with_equal_focus_colors(metrics: ModalMetrics) -> ModalTheme {
        ModalTheme::new(
            ModalPaint::new(
                rgba(0x00000099),
                rgba(0x202024ff),
                rgba(0x606068ff),
                rgba(0xffffffff),
                rgba(0xb0b0b8ff),
                rgba(0x505058ff),
                rgba(0x404048ff),
                rgba(0x606068ff),
                rgba(0x606068ff),
                rgba(0x5599ffff),
                rgba(0x5599ff22),
                rgba(0xffbb55ff),
                rgba(0xffbb5522),
                rgba(0xff6677ff),
                rgba(0xff667722),
            ),
            metrics,
        )
    }

    fn install_test_catalogs(cx: &mut TestAppContext) {
        cx.set_global(test_button_theme());
        cx.set_global(test_menu_theme());
        let input_paint = TextInputPaint::new(
            rgba(0xffffffff),
            rgba(0x888888ff),
            rgba(0x3355aaff),
            rgba(0xffffffff),
            rgba(0x777777ff),
            rgba(0x555555ff),
        );
        cx.set_global(TextInputTheme::new(
            TextInputVariants::new(input_paint, input_paint),
            TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(20.0)),
        ));
        cx.update(crate::menu::init);
        cx.update(crate::text_input::init);
        cx.update(crate::tooltip::init);
        cx.update(super::init);
        cx.update(|cx| install_text_input_keybindings(cx, TextInputKeybindingProfile::MacOs));
        cx.update(|cx| install_modal_policy(cx, ModalDesktopPolicy::mac_os()));
        cx.update(|cx| install_modal_keybindings(cx, ModalKeybindingProfile::MacOs));
        cx.update(|cx| {
            install_modal_theme(
                cx,
                test_modal_theme(ModalMetrics::new(px(360.0), px(480.0), px(640.0))),
            )
        });
    }

    struct AlertFixture {
        invoker: FocusHandle,
        underlay_activations: Rc<Cell<usize>>,
        outcome: Rc<RefCell<Option<AlertOutcome<&'static str>>>>,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl AlertFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let outcome = self.outcome.clone();
            self.presentation = Some(
                Alert::new(
                    ModalId::new("render-alert"),
                    "Render alert",
                    "Save Changes",
                    "Choose whether to save the current changes.",
                    vec![
                        ModalAction::new("save", "Save", ModalActionRole::Affirmative, "save")
                            .default_action(true),
                        ModalAction::new("cancel", "Cancel", ModalActionRole::Cancel, "cancel"),
                    ],
                )
                .suppression(AlertSuppression::new("Do not ask again", false))
                .present(window, cx, move |result, _| {
                    *outcome.borrow_mut() = Some(result);
                })
                .expect("alert should present"),
            );
        }

        fn queue_progress(
            &mut self,
            id: &'static str,
            outcomes: Rc<RefCell<Vec<ProgressDialogOutcome>>>,
            lifecycle: Rc<RefCell<Vec<ModalLifecycleEvent>>>,
            window: &Window,
            cx: &mut Context<Self>,
        ) -> super::super::ProgressDialogHandle {
            ProgressDialog::new(
                ModalId::new(id),
                "Queued progress operation",
                "Queued Progress",
                "Waiting",
                ProgressState::Indeterminate,
                ProgressCancellation::Cancellable(ModalAction::new(
                    (),
                    "Cancel",
                    ModalActionRole::Cancel,
                    format!("{id}-cancel"),
                )),
            )
            .present_with_lifecycle(
                window,
                cx,
                |_, _, _| super::super::ProgressCancelDecision::Deny,
                move |outcome, _| outcomes.borrow_mut().push(outcome),
                move |event, _| lifecycle.borrow_mut().push(*event),
            )
            .expect("ProgressDialog should queue")
        }
    }

    impl Render for AlertFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activations = self.underlay_activations.clone();
            let down_leaks = self.underlay_activations.clone();
            let up_leaks = self.underlay_activations.clone();
            let move_leaks = self.underlay_activations.clone();
            let wheel_leaks = self.underlay_activations.clone();
            let key_leaks = self.underlay_activations.clone();
            ModalLayer::new(TooltipLayer::new(
                div()
                    .id("modal-underlay")
                    .size_full()
                    .track_focus(&self.invoker)
                    .on_mouse_down(gpui::MouseButton::Left, move |_, _, _| {
                        down_leaks.set(down_leaks.get() + 1)
                    })
                    .on_mouse_up(gpui::MouseButton::Left, move |_, _, _| {
                        up_leaks.set(up_leaks.get() + 1)
                    })
                    .on_mouse_move(move |_, _, _| move_leaks.set(move_leaks.get() + 1))
                    .on_scroll_wheel(move |_, _, _| wheel_leaks.set(wheel_leaks.get() + 1))
                    .on_key_down(move |_, _, _| key_leaks.set(key_leaks.get() + 1))
                    .child(
                        Button::new("modal-underlay-button", "Underlay")
                            .debug_selector("modal-underlay-button")
                            .on_activate(move |_, _, _| activations.set(activations.get() + 1)),
                    ),
            ))
        }
    }

    struct DeferredRestorationFixture {
        invoker: Option<FocusHandle>,
        unrelated: FocusHandle,
        invoker_focuses: Rc<Cell<usize>>,
        unrelated_focuses: Rc<Cell<usize>>,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl DeferredRestorationFixture {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let invoker = cx.focus_handle().tab_stop(true);
            let unrelated = cx.focus_handle().tab_stop(true);
            let invoker_focuses = Rc::new(Cell::new(0));
            let unrelated_focuses = Rc::new(Cell::new(0));
            let observed_invoker_focuses = invoker_focuses.clone();
            let observed_unrelated_focuses = unrelated_focuses.clone();
            cx.on_focus(&invoker, window, move |_, _, _| {
                observed_invoker_focuses.set(observed_invoker_focuses.get() + 1);
            })
            .detach();
            cx.on_focus(&unrelated, window, move |_, _, _| {
                observed_unrelated_focuses.set(observed_unrelated_focuses.get() + 1);
            })
            .detach();
            Self {
                invoker: Some(invoker),
                unrelated,
                invoker_focuses,
                unrelated_focuses,
                presentation: None,
            }
        }

        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.presentation = Some(
                Alert::new(
                    ModalId::new("deferred-restoration-alert"),
                    "Deferred focus restoration",
                    "Continue?",
                    "Focus restoration waits for Operating-System Window activation.",
                    vec![
                        ModalAction::new(
                            "continue",
                            "Continue",
                            ModalActionRole::Affirmative,
                            "deferred-restoration-continue",
                        )
                        .default_action(true),
                    ],
                )
                .present(window, cx, |_, _| {})
                .expect("deferred-restoration Alert should present"),
            );
        }
    }

    impl Render for DeferredRestorationFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let invoker = self.invoker.clone();
            ModalLayer::new(
                div()
                    .size_full()
                    .when_some(invoker, |underlay, invoker| {
                        underlay.child(
                            div()
                                .id("deferred-restoration-invoker")
                                .h(px(24.0))
                                .track_focus(&invoker),
                        )
                    })
                    .child(
                        div()
                            .id("deferred-restoration-unrelated")
                            .h(px(24.0))
                            .track_focus(&self.unrelated),
                    ),
            )
        }
    }

    struct ActionGeometryFixture {
        actions: Vec<ModalAction<&'static str>>,
        help: Option<ModalAction<&'static str>>,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl ActionGeometryFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let mut alert = Alert::new(
                ModalId::new("action-geometry"),
                "Action geometry",
                "Adaptive Actions",
                "Every required action remains reachable.",
                self.actions.clone(),
            );
            if let Some(help) = self.help.clone() {
                alert = alert.help_action(help);
            }
            self.presentation = Some(
                alert
                    .present(window, cx, |_, _| {})
                    .expect("geometry Alert should present"),
            );
        }
    }

    impl Render for ActionGeometryFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    struct DialogGeometryBody {
        tall: bool,
    }

    impl Render for DialogGeometryBody {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .debug_selector(|| "geometry-dialog-body-content".to_owned())
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(
                    div()
                        .debug_selector(|| "geometry-dialog-body-start".to_owned())
                        .w_full()
                        .min_w_0()
                        .h(px(28.0))
                        .child("Localized form content begins here."),
                )
                .when(self.tall, |body| body.child(div().w_full().h(px(360.0))))
                .child(
                    div()
                        .debug_selector(|| "geometry-dialog-body-end".to_owned())
                        .w_full()
                        .min_w_0()
                        .h(px(28.0))
                        .child("Every required field remains vertically reachable."),
                )
        }
    }

    struct DialogGeometryFixture {
        body: Entity<DialogGeometryBody>,
        title: SharedString,
        description: SharedString,
        actions: Vec<ModalAction<&'static str>>,
        size: DialogSize,
        presentation: Option<super::super::DialogCompletion>,
    }

    impl DialogGeometryFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.presentation = Some(
                Dialog::new(
                    ModalId::new("dialog-geometry"),
                    "Dialog constrained geometry",
                    self.title.clone(),
                    self.actions.clone(),
                    DialogInitialFocus::Action("cancel"),
                )
                .description(self.description.clone())
                .size(self.size)
                .body(self.body.clone())
                .present(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Deny {
                        first_invalid: None,
                    },
                    |_, _| {},
                )
                .expect("geometry Dialog should present"),
            );
        }
    }

    impl Render for DialogGeometryFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    struct LongAlertGeometryFixture {
        title: SharedString,
        message: SharedString,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl LongAlertGeometryFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.presentation = Some(
                Alert::new(
                    ModalId::new("long-alert-geometry"),
                    "Long localized Alert geometry",
                    self.title.clone(),
                    self.message.clone(),
                    vec![
                        ModalAction::new(
                            "continue",
                            "继续执行完整的本地化操作并保留所有当前设置",
                            ModalActionRole::Affirmative,
                            "long-alert-continue",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "取消此操作而不丢失任何已输入的信息",
                            ModalActionRole::Cancel,
                            "long-alert-cancel",
                        ),
                    ],
                )
                .present(window, cx, |_, _| {})
                .expect("long Alert should present"),
            );
        }
    }

    impl Render for LongAlertGeometryFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    fn open_action_geometry_window(
        cx: &mut TestAppContext,
        viewport: gpui::Size<gpui::Pixels>,
        direction: TextDirection,
        metrics: ModalMetrics,
        button_scale: f32,
        actions: Vec<ModalAction<&'static str>>,
        help: Option<ModalAction<&'static str>>,
    ) -> WindowHandle<ActionGeometryFixture> {
        install_test_catalogs(cx);
        cx.set_global(ModalDesktopPolicy::mac_os().with_text_direction(direction));
        cx.set_global(test_button_theme_scaled(button_scale));
        cx.set_global(test_modal_theme(metrics));
        cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        viewport,
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    cx.new(|_| ActionGeometryFixture {
                        actions,
                        help,
                        presentation: None,
                    })
                },
            )
            .unwrap_or_else(|error| panic!("action geometry window failed: {error}"))
        })
    }

    struct AlertIntentFixture {
        intent: super::super::AlertIntent,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl AlertIntentFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.presentation = Some(
                Alert::new(
                    ModalId::new("intent-treatment"),
                    "Intent treatment",
                    "Notice",
                    "The semantic intent is presented without a caller accessory.",
                    vec![
                        ModalAction::new("ok", "OK", ModalActionRole::Affirmative, "intent-ok")
                            .default_action(true),
                    ],
                )
                .intent(self.intent)
                .present(window, cx, |_, _| {})
                .expect("intent Alert should present"),
            );
        }
    }

    impl Render for AlertIntentFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    struct ProgressGeometryFixture {
        handle: Option<super::super::ProgressDialogHandle>,
    }

    impl ProgressGeometryFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.handle = Some(
                ProgressDialog::new(
                    ModalId::new("stable-progress-geometry"),
                    "Stable progress geometry",
                    "Updating Project",
                    "Starting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        (),
                        "Cancel",
                        ModalActionRole::Cancel,
                        "stable-cancel",
                    )),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Deny,
                    |_, _| {},
                )
                .expect("progress geometry should present"),
            );
        }
    }

    impl Render for ProgressGeometryFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    fn open_alert_intent_window(
        cx: &mut TestAppContext,
        intent: super::super::AlertIntent,
    ) -> WindowHandle<AlertIntentFixture> {
        install_test_catalogs(cx);
        cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(600.0), px(500.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    cx.new(|_| AlertIntentFixture {
                        intent,
                        presentation: None,
                    })
                },
            )
            .unwrap_or_else(|error| panic!("intent Alert window failed: {error}"))
        })
    }

    fn bounds_contains(outer: Bounds<gpui::Pixels>, inner: Bounds<gpui::Pixels>) -> bool {
        inner.left() >= outer.left()
            && inner.right() <= outer.right()
            && inner.top() >= outer.top()
            && inner.bottom() <= outer.bottom()
    }

    fn bounds_horizontally_contain(
        outer: Bounds<gpui::Pixels>,
        inner: Bounds<gpui::Pixels>,
    ) -> bool {
        inner.left() >= outer.left() && inner.right() <= outer.right()
    }

    type AlertWindow<'a> = (
        Entity<AlertFixture>,
        Rc<Cell<usize>>,
        Rc<RefCell<Option<AlertOutcome<&'static str>>>>,
        &'a mut VisualTestContext,
    );

    fn alert_window(cx: &mut TestAppContext) -> AlertWindow<'_> {
        install_test_catalogs(cx);
        let underlay = Rc::new(Cell::new(0));
        let outcome = Rc::new(RefCell::new(None));
        let root_underlay = underlay.clone();
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: root_underlay,
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            let invoker = root.read(cx).invoker.clone();
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        (root, underlay, outcome, cx)
    }

    #[gpui::test]
    fn standard_default_action_receives_macos_ring_without_changing_standard_emphasis(
        cx: &mut TestAppContext,
    ) {
        let window = open_action_geometry_window(
            cx,
            size(px(600.0), px(500.0)),
            TextDirection::LeftToRight,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new(
                    "secondary",
                    "Later",
                    ModalActionRole::Auxiliary,
                    "standard-secondary",
                )
                .with_emphasis(ModalActionEmphasis::Standard),
                ModalAction::new(
                    "default",
                    "Continue",
                    ModalActionRole::Affirmative,
                    "standard-default",
                )
                .with_emphasis(ModalActionEmphasis::Standard)
                .default_action(true),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let default = cx
            .debug_bounds("modal-action-standard-default")
            .expect("standard default should render");
        let ring = cx
            .debug_bounds("modal-action-default-ring-standard-default")
            .expect("macOS default ring should render");

        assert!(
            bounds_contains(ring, default)
                && cx
                    .debug_bounds("modal-action-default-ring-standard-secondary")
                    .is_none(),
            "standard default ring {ring:?} did not distinguish default {default:?}"
        );
    }

    #[gpui::test]
    fn focus_geometry_remains_visible_when_focus_normal_and_default_colors_match(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        cx.set_global(test_button_theme_scaled_with_focus(1.0, rgba(0x606060ff)));
        cx.set_global(test_modal_theme_with_equal_focus_colors(ModalMetrics::new(
            px(360.0),
            px(480.0),
            px(640.0),
        )));
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| {
                let outcome = root.outcome.clone();
                root.presentation = Some(
                    Alert::new(
                        ModalId::new("equal-focus-alert"),
                        "Equal focus colors",
                        "Save Changes",
                        "Keyboard focus must remain visible without a color difference.",
                        vec![
                            ModalAction::new(
                                "save",
                                "Save",
                                ModalActionRole::Affirmative,
                                "equal-focus-save",
                            )
                            .default_action(true),
                        ],
                    )
                    .suppression(AlertSuppression::new("Do not ask again", false))
                    .present(window, cx, move |result, _| {
                        *outcome.borrow_mut() = Some(result);
                    })
                    .expect("equal-color Alert should present"),
                );
            });
        });
        cx.run_until_parked();

        let action = cx
            .debug_bounds("modal-action-equal-focus-save")
            .expect("default action should render");
        let action_focus = cx
            .debug_bounds("modal-action-equal-focus-save-keyboard-focus")
            .expect("focused action should add inner geometry");
        let default_ring = cx
            .debug_bounds("modal-action-default-ring-equal-focus-save")
            .expect("default designation should remain independently rendered");
        let suppression = cx
            .debug_bounds("modal-alert-suppression")
            .expect("suppression should render");
        let suppression_was_unfocused = cx
            .debug_bounds("modal-alert-suppression-keyboard-focus")
            .is_none();

        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        let suppression_focus = cx
            .debug_bounds("modal-alert-suppression-keyboard-focus")
            .expect("focused suppression should add inner geometry");

        let retained_default_ring = cx.debug_bounds("modal-action-default-ring-equal-focus-save");
        assert!(
            suppression_was_unfocused
                && bounds_contains(action, action_focus)
                && action != action_focus
                && bounds_contains(suppression, suppression_focus)
                && suppression != suppression_focus
                && retained_default_ring == Some(default_ring),
            "suppression_was_unfocused={suppression_was_unfocused}, action={action:?}, action_focus={action_focus:?}, suppression={suppression:?}, suppression_focus={suppression_focus:?}, default_ring={default_ring:?}, retained_default_ring={retained_default_ring:?}"
        );
    }

    #[gpui::test]
    fn informational_alert_renders_bounded_semantic_treatment_without_accessory(
        cx: &mut TestAppContext,
    ) {
        let window = open_alert_intent_window(cx, super::super::AlertIntent::Informational);
        let root = window.root(cx).expect("intent Alert root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let treatment = cx
            .debug_bounds("modal-alert-intent-informational")
            .expect("informational treatment should render");
        let marker = cx
            .debug_bounds("modal-alert-intent-mark-informational")
            .expect("informational marker should render");

        assert!(bounds_contains(treatment, marker) && marker.size.width > px(0.0));
    }

    #[gpui::test]
    fn warning_alert_renders_bounded_semantic_treatment_without_accessory(cx: &mut TestAppContext) {
        let window = open_alert_intent_window(cx, super::super::AlertIntent::Warning);
        let root = window.root(cx).expect("intent Alert root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let treatment = cx
            .debug_bounds("modal-alert-intent-warning")
            .expect("warning treatment should render");
        let marker = cx
            .debug_bounds("modal-alert-intent-mark-warning")
            .expect("warning marker should render");

        assert!(bounds_contains(treatment, marker) && marker.size.width > px(0.0));
    }

    #[gpui::test]
    fn critical_alert_renders_bounded_semantic_treatment_without_accessory(
        cx: &mut TestAppContext,
    ) {
        let window = open_alert_intent_window(cx, super::super::AlertIntent::Critical);
        let root = window.root(cx).expect("intent Alert root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let treatment = cx
            .debug_bounds("modal-alert-intent-critical")
            .expect("critical treatment should render");
        let marker = cx
            .debug_bounds("modal-alert-intent-mark-critical")
            .expect("critical marker should render");

        assert!(bounds_contains(treatment, marker) && marker.size.width > px(0.0));
    }

    #[test]
    fn alert_intent_presentations_use_distinct_markers_and_semantic_paint() {
        let paint = test_modal_theme(ModalMetrics::new(px(360.0), px(480.0), px(640.0))).paint;
        let informational =
            alert_intent_presentation(super::super::AlertIntent::Informational, paint);
        let warning = alert_intent_presentation(super::super::AlertIntent::Warning, paint);
        let critical = alert_intent_presentation(super::super::AlertIntent::Critical, paint);

        assert!(
            informational.marker != warning.marker
                && warning.marker != critical.marker
                && informational.accent != warning.accent
                && warning.accent != critical.accent
                && informational.background != warning.background
                && warning.background != critical.background
        );
    }

    #[gpui::test]
    fn long_help_label_forces_one_vertical_footer_without_escaping_it(cx: &mut TestAppContext) {
        let window = open_action_geometry_window(
            cx,
            size(px(560.0), px(700.0)),
            TextDirection::LeftToRight,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new(
                    "save",
                    "Save",
                    ModalActionRole::Affirmative,
                    "geometry-save",
                )
                .default_action(true),
                ModalAction::new(
                    "cancel",
                    "Cancel",
                    ModalActionRole::Cancel,
                    "geometry-cancel",
                ),
            ],
            Some(ModalAction::new(
                "help",
                "Open detailed assistance for resolving this localized operation safely",
                ModalActionRole::Help,
                "geometry-help",
            )),
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let actions = [
            "modal-action-geometry-help",
            "modal-action-geometry-save",
            "modal-action-geometry-cancel",
        ]
        .map(|selector| cx.debug_bounds(selector).expect("action should render"));

        assert!(
            actions
                .into_iter()
                .all(|action| bounds_contains(footer, action)),
            "actions {actions:?} escaped footer {footer:?}"
        );
    }

    #[gpui::test]
    fn long_cjk_decision_labels_wrap_inside_growing_hitboxes(cx: &mut TestAppContext) {
        let window = open_action_geometry_window(
            cx,
            size(px(560.0), px(700.0)),
            TextDirection::LeftToRight,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new(
                    "save",
                    "変更内容を安全に保存して作業を続ける",
                    ModalActionRole::Affirmative,
                    "cjk-save",
                )
                .default_action(true),
                ModalAction::new(
                    "replace",
                    "既存の設定を確認してから置き換える",
                    ModalActionRole::Auxiliary,
                    "cjk-replace",
                ),
                ModalAction::new(
                    "cancel",
                    "変更を破棄せずにこの操作を取り消す",
                    ModalActionRole::Cancel,
                    "cjk-cancel",
                ),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let actions = [
            "modal-action-cjk-save",
            "modal-action-cjk-replace",
            "modal-action-cjk-cancel",
        ]
        .map(|selector| cx.debug_bounds(selector).expect("CJK action should render"));
        let ordinary_height = px(24.0);

        assert!(
            actions
                .iter()
                .all(|action| bounds_contains(footer, *action))
                && actions
                    .iter()
                    .any(|action| action.size.height > ordinary_height),
            "CJK action hitboxes {actions:?} did not wrap inside {footer:?}"
        );
    }

    #[gpui::test]
    fn right_to_left_horizontal_footer_mirrors_groups_and_decision_order(cx: &mut TestAppContext) {
        let window = open_action_geometry_window(
            cx,
            size(px(640.0), px(700.0)),
            TextDirection::RightToLeft,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new("save", "Save", ModalActionRole::Affirmative, "rtl-save")
                    .default_action(true),
                ModalAction::new("cancel", "Cancel", ModalActionRole::Cancel, "rtl-cancel"),
            ],
            Some(ModalAction::new(
                "help",
                "Help",
                ModalActionRole::Help,
                "rtl-help",
            )),
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let save = cx
            .debug_bounds("modal-action-rtl-save")
            .expect("Save should render");
        let cancel = cx
            .debug_bounds("modal-action-rtl-cancel")
            .expect("Cancel should render");
        let help = cx
            .debug_bounds("modal-action-rtl-help")
            .expect("Help should render");

        assert!(
            save.left() < cancel.left() && cancel.right() < help.left(),
            "RTL bounds were save={save:?}, cancel={cancel:?}, help={help:?}"
        );
    }

    #[gpui::test]
    fn left_to_right_horizontal_footer_without_help_anchors_decisions_at_logical_trailing(
        cx: &mut TestAppContext,
    ) {
        let metrics = ModalMetrics::new(px(440.0), px(480.0), px(640.0));
        let window = open_action_geometry_window(
            cx,
            size(px(640.0), px(700.0)),
            TextDirection::LeftToRight,
            metrics,
            1.0,
            vec![
                ModalAction::new(
                    "save",
                    "Save",
                    ModalActionRole::Affirmative,
                    "ltr-edge-save",
                )
                .default_action(true),
                ModalAction::new(
                    "cancel",
                    "Cancel",
                    ModalActionRole::Cancel,
                    "ltr-edge-cancel",
                ),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let save = cx
            .debug_bounds("modal-action-ltr-edge-save")
            .expect("Save should render");
        let trailing_gap = footer.right() - save.right();

        assert!(
            trailing_gap >= metrics.surface_padding
                && trailing_gap <= metrics.surface_padding + px(1.0),
            "LTR decisions had trailing gap {trailing_gap:?} inside {footer:?}"
        );
    }

    #[gpui::test]
    fn right_to_left_horizontal_footer_without_help_anchors_decisions_at_logical_trailing(
        cx: &mut TestAppContext,
    ) {
        let metrics = ModalMetrics::new(px(440.0), px(480.0), px(640.0));
        let window = open_action_geometry_window(
            cx,
            size(px(640.0), px(700.0)),
            TextDirection::RightToLeft,
            metrics,
            1.0,
            vec![
                ModalAction::new(
                    "save",
                    "Save",
                    ModalActionRole::Affirmative,
                    "rtl-edge-save",
                )
                .default_action(true),
                ModalAction::new(
                    "cancel",
                    "Cancel",
                    ModalActionRole::Cancel,
                    "rtl-edge-cancel",
                ),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let save = cx
            .debug_bounds("modal-action-rtl-edge-save")
            .expect("Save should render");
        let trailing_gap = save.left() - footer.left();

        assert!(
            trailing_gap >= metrics.surface_padding
                && trailing_gap <= metrics.surface_padding + px(1.0),
            "RTL decisions had trailing gap {trailing_gap:?} inside {footer:?}"
        );
    }

    #[gpui::test]
    fn short_compact_dialog_follows_content_below_its_height_cap(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));
        cx.set_global(test_modal_theme(metrics));
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(800.0), px(700.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    let body = cx.new(|_| DialogGeometryBody { tall: false });
                    cx.new(|_| DialogGeometryFixture {
                        body,
                        title: "Compact Dialog".into(),
                        description: "A short scoped task.".into(),
                        actions: vec![
                            ModalAction::new(
                                "save",
                                "Save",
                                ModalActionRole::Affirmative,
                                "short-dialog-save",
                            )
                            .default_action(true),
                            ModalAction::new(
                                "cancel",
                                "Cancel",
                                ModalActionRole::Cancel,
                                "short-dialog-cancel",
                            ),
                        ],
                        size: DialogSize::Compact,
                        presentation: None,
                    })
                },
            )
            .unwrap_or_else(|error| panic!("short Dialog window failed: {error}"))
        });
        let root = window.root(cx).expect("short Dialog root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("Dialog surface should render");
        let header = cx
            .debug_bounds("modal-header")
            .expect("Dialog header should render");
        let body = cx
            .debug_bounds("modal-body-viewport")
            .expect("Dialog body should render");
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("Dialog footer should render");

        assert!(
            surface.size.height < metrics.dialog_height_cap()
                && bounds_contains(surface, header)
                && bounds_contains(surface, body)
                && bounds_contains(surface, footer)
                && header.bottom() <= body.top()
                && body.bottom() <= footer.top(),
            "surface={surface:?}, header={header:?}, body={body:?}, footer={footer:?}, cap={:?}",
            metrics.dialog_height_cap()
        );
    }

    #[gpui::test]
    fn short_progress_dialog_follows_content_below_its_height_cap(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0));
        cx.set_global(test_modal_theme(metrics));
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(800.0), px(700.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| cx.new(|_| ProgressGeometryFixture { handle: None }),
            )
            .unwrap_or_else(|error| panic!("short ProgressDialog window failed: {error}"))
        });
        let root = window
            .root(cx)
            .expect("short ProgressDialog root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("ProgressDialog surface should render");
        let header = cx
            .debug_bounds("modal-header")
            .expect("ProgressDialog header should render");
        let body = cx
            .debug_bounds("modal-body-viewport")
            .expect("ProgressDialog body should render");
        let status = cx
            .debug_bounds("modal-progress-status")
            .expect("ProgressDialog status should render");
        let detail = cx
            .debug_bounds("modal-progress-detail")
            .expect("ProgressDialog detail region should render");
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("ProgressDialog footer should render");

        assert!(
            surface.size.height < metrics.progress_height_cap()
                && bounds_contains(surface, header)
                && bounds_contains(surface, body)
                && bounds_contains(surface, footer)
                && bounds_contains(body, status)
                && bounds_contains(body, detail),
            "surface={surface:?}, header={header:?}, body={body:?}, status={status:?}, detail={detail:?}, footer={footer:?}, cap={:?}",
            metrics.progress_height_cap()
        );
    }

    #[gpui::test]
    fn constrained_scaled_dialog_reaches_long_header_body_and_every_action_vertically(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0)).scaled(1.5);
        cx.set_global(test_button_theme_scaled(1.5));
        cx.set_global(test_modal_theme(metrics));
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(320.0), px(240.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    let body = cx.new(|_| DialogGeometryBody { tall: true });
                    cx.new(|_| DialogGeometryFixture {
                        body,
                        title: "保存する前にすべての設定内容を確認してください".into(),
                        description:
                            "入力した値を保持したまま、必要な項目と選択肢を最後まで確認できます。"
                                .into(),
                        actions: vec![
                            ModalAction::new(
                                "save",
                                "すべての変更内容を安全に保存して続ける",
                                ModalActionRole::Affirmative,
                                "constrained-dialog-save",
                            )
                            .default_action(true),
                            ModalAction::new(
                                "review",
                                "既存の設定をもう一度詳しく確認する",
                                ModalActionRole::Auxiliary,
                                "constrained-dialog-review",
                            ),
                            ModalAction::new(
                                "cancel",
                                "入力内容を失わずにこの操作を取り消す",
                                ModalActionRole::Cancel,
                                "constrained-dialog-cancel",
                            ),
                        ],
                        size: DialogSize::Compact,
                        presentation: None,
                    })
                },
            )
            .unwrap_or_else(|error| panic!("constrained Dialog window failed: {error}"))
        });
        let root = window
            .root(cx)
            .expect("constrained Dialog root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("Dialog surface should render");
        let header = cx
            .debug_bounds("modal-header")
            .expect("Dialog header should render");
        let body = cx
            .debug_bounds("modal-body-viewport")
            .expect("Dialog body should render");
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("Dialog footer should render");
        assert!(
            bounds_contains(surface, header)
                && bounds_contains(surface, body)
                && bounds_contains(surface, footer)
                && body.size.height > px(0.0)
                && header.bottom() <= body.top()
                && body.bottom() <= footer.top(),
            "surface={surface:?}, header={header:?}, body={body:?}, footer={footer:?}"
        );

        let mut header_top_reached = [false; 2];
        let mut header_bottom_reached = [false; 2];
        for _ in 0..20 {
            for ((top_reached, bottom_reached), selector) in header_top_reached
                .iter_mut()
                .zip(header_bottom_reached.iter_mut())
                .zip(["modal-header-title", "modal-header-description"])
            {
                let content = cx
                    .debug_bounds(selector)
                    .expect("header content should render");
                assert!(
                    bounds_horizontally_contain(header, content),
                    "{selector} escaped horizontally: {content:?} outside {header:?}"
                );
                *top_reached |= content.top() >= header.top() && content.top() < header.bottom();
                *bottom_reached |=
                    content.bottom() > header.top() && content.bottom() <= header.bottom();
            }
            cx.simulate_event(ScrollWheelEvent {
                position: header.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-12.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        let mut body_top_reached = [false; 2];
        let mut body_bottom_reached = [false; 2];
        for _ in 0..40 {
            for ((top_reached, bottom_reached), selector) in body_top_reached
                .iter_mut()
                .zip(body_bottom_reached.iter_mut())
                .zip(["geometry-dialog-body-start", "geometry-dialog-body-end"])
            {
                let content = cx
                    .debug_bounds(selector)
                    .expect("body marker should render");
                assert!(
                    bounds_horizontally_contain(body, content),
                    "{selector} escaped horizontally: {content:?} outside {body:?}"
                );
                *top_reached |= content.top() >= body.top() && content.top() < body.bottom();
                *bottom_reached |=
                    content.bottom() > body.top() && content.bottom() <= body.bottom();
            }
            cx.simulate_event(ScrollWheelEvent {
                position: body.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-12.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        let action_selectors = [
            "modal-action-constrained-dialog-save",
            "modal-action-constrained-dialog-review",
            "modal-action-constrained-dialog-cancel",
        ];
        let mut actions_reached = [false; 3];
        for _ in 0..12 {
            for (reached, selector) in actions_reached.iter_mut().zip(action_selectors) {
                let action = cx
                    .debug_bounds(selector)
                    .expect("Dialog action should render");
                assert!(
                    bounds_horizontally_contain(footer, action),
                    "{selector} escaped horizontally: {action:?} outside {footer:?}"
                );
                *reached |= bounds_contains(footer, action);
            }
            cx.simulate_event(ScrollWheelEvent {
                position: footer.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-64.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        assert!(
            header_top_reached.into_iter().all(|reached| reached)
                && header_bottom_reached.into_iter().all(|reached| reached)
                && body_top_reached.into_iter().all(|reached| reached)
                && body_bottom_reached.into_iter().all(|reached| reached)
                && actions_reached.into_iter().all(|reached| reached),
            "header_top={header_top_reached:?}, header_bottom={header_bottom_reached:?}, body_top={body_top_reached:?}, body_bottom={body_bottom_reached:?}, actions={actions_reached:?}"
        );
    }

    #[gpui::test]
    fn constrained_scaled_alert_reaches_long_title_message_and_actions_vertically(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let metrics = ModalMetrics::new(px(360.0), px(480.0), px(640.0)).scaled(1.5);
        cx.set_global(test_button_theme_scaled(1.5));
        cx.set_global(test_modal_theme(metrics));
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(300.0), px(230.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| {
                    cx.new(|_| LongAlertGeometryFixture {
                        title: "続行する前に重要な変更内容をすべて確認してください".into(),
                        message: "この操作には複数の重要な手順が含まれています。表示されている情報を最後まで確認してから、続行するか取り消すかを選択してください。入力済みの情報は取り消しても保持されます。"
                            .into(),
                        presentation: None,
                    })
                },
            )
            .unwrap_or_else(|error| panic!("constrained Alert window failed: {error}"))
        });
        let root = window
            .root(cx)
            .expect("constrained Alert root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("Alert surface should render");
        let header = cx
            .debug_bounds("modal-header")
            .expect("Alert header should render");
        let body = cx
            .debug_bounds("modal-body-viewport")
            .expect("Alert body should render");
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("Alert footer should render");
        assert!(
            bounds_contains(surface, header)
                && bounds_contains(surface, body)
                && bounds_contains(surface, footer)
                && body.size.height > px(0.0),
            "surface={surface:?}, header={header:?}, body={body:?}, footer={footer:?}"
        );

        let mut title_top_reached = false;
        let mut title_bottom_reached = false;
        for _ in 0..20 {
            let title = cx
                .debug_bounds("modal-header-title")
                .expect("Alert title should render");
            assert!(bounds_horizontally_contain(header, title));
            title_top_reached |= title.top() >= header.top() && title.top() < header.bottom();
            title_bottom_reached |=
                title.bottom() > header.top() && title.bottom() <= header.bottom();
            cx.simulate_event(ScrollWheelEvent {
                position: header.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-12.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        let mut message_top_reached = false;
        let mut message_bottom_reached = false;
        for _ in 0..32 {
            let message = cx
                .debug_bounds("modal-alert-message")
                .expect("Alert message should render");
            assert!(
                bounds_horizontally_contain(body, message),
                "message escaped horizontally: {message:?} outside {body:?}"
            );
            message_top_reached |= message.top() >= body.top() && message.top() < body.bottom();
            message_bottom_reached |=
                message.bottom() > body.top() && message.bottom() <= body.bottom();
            cx.simulate_event(ScrollWheelEvent {
                position: body.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-12.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        let selectors = [
            "modal-action-long-alert-continue",
            "modal-action-long-alert-cancel",
        ];
        let mut actions_reached = [false; 2];
        for _ in 0..12 {
            for (reached, selector) in actions_reached.iter_mut().zip(selectors) {
                let action = cx
                    .debug_bounds(selector)
                    .expect("Alert action should render");
                assert!(bounds_horizontally_contain(footer, action));
                *reached |= bounds_contains(footer, action);
            }
            cx.simulate_event(ScrollWheelEvent {
                position: footer.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-64.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        assert!(
            title_top_reached
                && title_bottom_reached
                && message_top_reached
                && message_bottom_reached
                && actions_reached.into_iter().all(|reached| reached),
            "title_top={title_top_reached}, title_bottom={title_bottom_reached}, message_top={message_top_reached}, message_bottom={message_bottom_reached}, actions={actions_reached:?}"
        );
    }

    #[gpui::test]
    fn tiny_width_keeps_wrapped_action_hitboxes_inside_the_footer(cx: &mut TestAppContext) {
        let window = open_action_geometry_window(
            cx,
            size(px(220.0), px(600.0)),
            TextDirection::LeftToRight,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new(
                    "continue",
                    "Continue safely",
                    ModalActionRole::Affirmative,
                    "tiny-continue",
                )
                .default_action(true),
                ModalAction::new(
                    "cancel",
                    "Cancel operation",
                    ModalActionRole::Cancel,
                    "tiny-cancel",
                ),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let actions = ["modal-action-tiny-continue", "modal-action-tiny-cancel"].map(|selector| {
            cx.debug_bounds(selector)
                .expect("tiny action should render")
        });

        assert!(
            actions
                .into_iter()
                .all(|action| bounds_contains(footer, action)),
            "tiny action bounds {actions:?} escaped {footer:?}"
        );
    }

    #[gpui::test]
    fn scaled_metrics_keep_actions_wrapped_and_reachable(cx: &mut TestAppContext) {
        let metrics = ModalMetrics::new(px(440.0), px(480.0), px(640.0)).scaled(1.75);
        let window = open_action_geometry_window(
            cx,
            size(px(900.0), px(1000.0)),
            TextDirection::LeftToRight,
            metrics,
            1.75,
            vec![
                ModalAction::new(
                    "continue",
                    "Continue with the complete operation",
                    ModalActionRole::Affirmative,
                    "scaled-continue",
                )
                .default_action(true),
                ModalAction::new(
                    "cancel",
                    "Cancel without losing entered values",
                    ModalActionRole::Cancel,
                    "scaled-cancel",
                ),
            ],
            None,
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let footer = cx
            .debug_bounds("modal-footer-1")
            .expect("footer should render");
        let actions =
            ["modal-action-scaled-continue", "modal-action-scaled-cancel"].map(|selector| {
                cx.debug_bounds(selector)
                    .expect("scaled action should render")
            });

        assert!(
            actions
                .iter()
                .all(|action| bounds_contains(footer, *action))
                && actions.iter().all(|action| action.size.height >= px(42.0)),
            "scaled action bounds {actions:?} escaped {footer:?}"
        );
    }

    #[gpui::test]
    fn short_height_footer_scroll_reaches_every_action_without_horizontal_escape(
        cx: &mut TestAppContext,
    ) {
        let window = open_action_geometry_window(
            cx,
            size(px(520.0), px(180.0)),
            TextDirection::LeftToRight,
            ModalMetrics::new(px(440.0), px(480.0), px(640.0)),
            1.0,
            vec![
                ModalAction::new(
                    "save",
                    "Save all localized changes and continue",
                    ModalActionRole::Affirmative,
                    "short-save",
                )
                .default_action(true),
                ModalAction::new(
                    "replace",
                    "Review and replace the existing configuration",
                    ModalActionRole::Auxiliary,
                    "short-replace",
                ),
                ModalAction::new(
                    "cancel",
                    "Cancel while retaining all entered values",
                    ModalActionRole::Cancel,
                    "short-cancel",
                ),
            ],
            Some(ModalAction::new(
                "help",
                "Open detailed help for this operation",
                ModalActionRole::Help,
                "short-help",
            )),
        );
        let root = window.root(cx).expect("geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let selectors = [
            "modal-action-short-help",
            "modal-action-short-save",
            "modal-action-short-replace",
            "modal-action-short-cancel",
        ];
        let mut reached = [false; 4];
        for _ in 0..24 {
            let footer = cx
                .debug_bounds("modal-footer-1")
                .expect("footer should render");
            for (reached, selector) in reached.iter_mut().zip(selectors) {
                let action = cx.debug_bounds(selector).expect("action should render");
                assert!(
                    action.left() >= footer.left() && action.right() <= footer.right(),
                    "{selector} escaped horizontally: {action:?} outside {footer:?}"
                );
                *reached |= bounds_contains(footer, action);
            }
            cx.simulate_event(ScrollWheelEvent {
                position: footer.center(),
                delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
            });
            cx.run_until_parked();
        }

        assert!(
            reached.into_iter().all(|reached| reached),
            "reached={reached:?}"
        );
    }

    #[gpui::test]
    fn determinate_thirty_five_percent_and_indeterminate_have_distinct_static_geometry(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(600.0), px(500.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| cx.new(|_| ProgressGeometryFixture { handle: None }),
            )
            .unwrap_or_else(|error| panic!("progress geometry window failed: {error}"))
        });
        let root = window
            .root(cx)
            .expect("progress geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let track = cx
            .debug_bounds("modal-progress-track")
            .expect("progress track should render");
        let stable_before = (
            cx.debug_bounds("modal-surface-1")
                .expect("progress surface should render"),
            cx.debug_bounds("modal-header")
                .expect("progress header should render"),
            cx.debug_bounds("modal-body-viewport")
                .expect("progress body should render"),
            cx.debug_bounds("modal-progress-status")
                .expect("progress status should render"),
            cx.debug_bounds("modal-progress-detail")
                .expect("progress detail region should render"),
            track,
            cx.debug_bounds("modal-footer-1")
                .expect("progress footer should render"),
        );
        let indeterminate = cx
            .debug_bounds("modal-progress-indeterminate")
            .expect("indeterminate treatment should render");
        let segments = [
            "modal-progress-indeterminate-segment-0",
            "modal-progress-indeterminate-segment-1",
            "modal-progress-indeterminate-segment-2",
            "modal-progress-indeterminate-segment-3",
        ]
        .map(|selector| {
            cx.debug_bounds(selector)
                .expect("indeterminate segment should render")
        });
        let handle = root
            .read_with(&cx, |root, _| root.handle.clone())
            .expect("progress handle should be retained");
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new()
                        .status("Processing the next bounded stage")
                        .detail(Some("The operation remains cancellable."))
                        .progress(ProgressState::Determinate(
                            DeterminateProgress::new(0.35)
                                .expect("thirty-five percent should be finite"),
                        )),
                    window,
                    cx,
                )
                .expect("determinate progress should update");
        });
        cx.run_until_parked();
        let determinate = cx
            .debug_bounds("modal-progress-determinate")
            .expect("determinate fill should render");
        let expected_width = track.size.width * 0.35;
        let stable_after = (
            cx.debug_bounds("modal-surface-1")
                .expect("progress surface should remain"),
            cx.debug_bounds("modal-header")
                .expect("progress header should remain"),
            cx.debug_bounds("modal-body-viewport")
                .expect("progress body should remain"),
            cx.debug_bounds("modal-progress-status")
                .expect("progress status should remain"),
            cx.debug_bounds("modal-progress-detail")
                .expect("progress detail region should remain"),
            cx.debug_bounds("modal-progress-track")
                .expect("progress track should remain"),
            cx.debug_bounds("modal-footer-1")
                .expect("progress footer should remain"),
        );

        assert!(
            stable_after == stable_before
                && indeterminate.size.width == track.size.width
                && segments
                    .windows(2)
                    .all(|pair| pair[0].right() < pair[1].left())
                && (determinate.size.width - expected_width).abs() < px(1.0)
                && determinate.size.width < indeterminate.size.width,
            "before={stable_before:?}, after={stable_after:?}, track={track:?}, indeterminate={indeterminate:?}, segments={segments:?}, determinate={determinate:?}"
        );
    }

    #[gpui::test]
    fn maximum_status_update_preserves_surface_track_and_cancel_bounds(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let window = cx.update(|cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                        point(px(0.0), px(0.0)),
                        size(px(600.0), px(500.0)),
                    ))),
                    ..WindowOptions::default()
                },
                |_, cx| cx.new(|_| ProgressGeometryFixture { handle: None }),
            )
            .unwrap_or_else(|error| panic!("progress geometry window failed: {error}"))
        });
        let root = window
            .root(cx)
            .expect("progress geometry root should exist");
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let before = (
            cx.debug_bounds("modal-surface-1")
                .expect("surface should render"),
            cx.debug_bounds("modal-progress-track")
                .expect("track should render"),
            cx.debug_bounds("modal-action-stable-cancel")
                .expect("Cancel should render"),
        );
        let handle = root
            .read_with(&cx, |root, _| root.handle.clone())
            .expect("progress handle should be retained");
        let maximum_multiline_status = "进\n".repeat(MAX_PROGRESS_STATUS_CHARACTERS / 2);
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().status(maximum_multiline_status),
                    window,
                    cx,
                )
                .expect("maximum accepted status should update");
        });
        cx.run_until_parked();
        let after = (
            cx.debug_bounds("modal-surface-1")
                .expect("surface should remain"),
            cx.debug_bounds("modal-progress-track")
                .expect("track should remain"),
            cx.debug_bounds("modal-action-stable-cancel")
                .expect("Cancel should remain"),
        );

        assert_eq!(after, before);
    }

    #[gpui::test]
    fn modal_initial_focus_and_space_activate_the_policy_target(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);

        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
        });
        cx.run_until_parked();

        assert!(
            matches!(
                outcome.borrow().as_ref(),
                Some(AlertOutcome::Activated {
                    action_id: "save",
                    source: ModalActivationSource::Space,
                    ..
                })
            ),
            "unexpected outcome: {:?}",
            outcome.borrow().as_ref()
        );
    }

    #[gpui::test]
    fn destructive_alert_focuses_safe_cancel_and_space_activates_it(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.read(cx).invoker.focus(window);
            root.update(cx, |root, cx| {
                let outcome = root.outcome.clone();
                root.presentation = Some(
                    Alert::new(
                        ModalId::new("destructive-alert-focus"),
                        "Delete selected workspace",
                        "Delete Workspace",
                        "This action cannot be undone.",
                        vec![
                            ModalAction::new(
                                "delete",
                                "Delete",
                                ModalActionRole::Affirmative,
                                "destructive-alert-delete",
                            )
                            .with_intent(ModalActionIntent::Destructive),
                            ModalAction::new(
                                "cancel",
                                "Cancel",
                                ModalActionRole::Cancel,
                                "destructive-alert-cancel",
                            ),
                        ],
                    )
                    .present(window, cx, move |result, _| {
                        *outcome.borrow_mut() = Some(result);
                    })
                    .expect("destructive Alert should present"),
                );
            });
        });
        cx.run_until_parked();

        press_space(cx);

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Activated {
                action_id: "cancel",
                source: ModalActivationSource::Space,
                ..
            })
        ));
    }

    #[gpui::test]
    fn cancellable_progress_focuses_cancel_and_space_requests_cancellation(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.read(cx).invoker.focus(window);
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("cancellable-progress-focus"),
                    "Cancelling file update",
                    "Updating Files",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "cancellable-progress-cancel",
                    )),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Allow,
                    move |result, _| *root_outcome.borrow_mut() = Some(result),
                )
                .expect("cancellable ProgressDialog should present");
            });
        });
        cx.run_until_parked();

        press_space(cx);

        assert_eq!(
            *outcome.borrow(),
            Some(ProgressDialogOutcome::Cancelled {
                source: ModalActivationSource::Space,
            })
        );
    }

    #[gpui::test]
    fn initially_disabled_progress_cancellation_is_inert_then_focuses_and_routes_after_enable(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let root_outcomes = outcomes.clone();
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let handle = cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("initially-disabled-progress"),
                    "Cancellation starts unavailable",
                    "Preparing Work",
                    "Waiting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(
                        ModalAction::new(
                            (),
                            "Cancel",
                            ModalActionRole::Cancel,
                            "initially-disabled-cancel",
                        )
                        .enabled(false),
                    ),
                )
                .present(
                    window,
                    cx,
                    move |source, _, _| {
                        root_requests.borrow_mut().push(source);
                        ProgressCancelDecision::Allow
                    },
                    move |outcome, _| root_outcomes.borrow_mut().push(outcome),
                )
                .expect("initially disabled cancellable ProgressDialog should present")
            })
        });
        cx.run_until_parked();
        let cancel = cx
            .debug_bounds("modal-action-initially-disabled-cancel")
            .expect("disabled Cancel action should render");

        cx.simulate_click(cancel.center(), Modifiers::default());
        cx.simulate_keystrokes("escape");
        press_space(cx);
        let initial = cx.update(|window, cx| {
            (
                super::super::core::active_progress_for_test(window, cx),
                super::super::core::active_progress_presentation_facts_for_test(window, cx),
                super::super::window_modal_is_open(window, cx),
            )
        });

        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(true),
                    window,
                    cx,
                )
                .expect("retained handle should enable cancellation");
        });
        cx.run_until_parked();
        press_space(cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(
            requests.borrow().as_slice() == [ModalActivationSource::Space]
                && outcomes.borrow().as_slice()
                    == [ProgressDialogOutcome::Cancelled {
                        source: ModalActivationSource::Space,
                    }]
                && initial.0.is_some_and(|(_, progress, generation)| {
                    progress.cancellation_capable
                        && !progress.cancellation_enabled
                        && generation == 0
                })
                && initial.1 == Some((false, 1))
                && initial.2
                && !cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );
    }

    #[gpui::test]
    fn programmatic_progress_surface_owns_contained_focus_without_underlay_escape(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let invoker = root.read_with(cx, |root, _| root.invoker.clone());
        let handle = cx.update(|window, cx| {
            window.activate_window();
            invoker.focus(window);
            root.update(cx, |_, cx| {
                ProgressDialog::<()>::new(
                    ModalId::new("programmatic-progress-focus"),
                    "Completing required work",
                    "Completing Work",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::programmatic_only(Duration::from_secs(30)),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Deny,
                    |_, _| {},
                )
                .expect("programmatic ProgressDialog should present")
            })
        });
        cx.run_until_parked();

        let initial_is_contained = cx.update(|window, cx| {
            super::super::core::focused_modal_parent(window, cx).is_some()
                && !invoker.is_focused(window)
        });
        cx.simulate_keystrokes("tab shift-tab escape");
        press_space(cx);
        let remains_contained = cx.update(|window, cx| {
            super::super::core::focused_modal_parent(window, cx).is_some()
                && !invoker.is_focused(window)
                && super::super::window_modal_is_open(window, cx)
        });

        assert!(initial_is_contained && remains_contained);
        cx.update(|window, cx| {
            handle
                .complete(window, cx)
                .expect("programmatic ProgressDialog should complete");
        });
    }

    #[gpui::test]
    fn modal_survives_when_the_caller_does_not_retain_its_dismissal_handle(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                let outcome = root.outcome.clone();
                Alert::new(
                    ModalId::new("unretained-alert"),
                    "Unretained alert",
                    "Unretained Alert",
                    "The window-owned presentation must retain itself.",
                    vec![
                        ModalAction::new("ok", "OK", ModalActionRole::Affirmative, "ok")
                            .default_action(true),
                    ],
                )
                .present(window, cx, move |result, _| {
                    *outcome.borrow_mut() = Some(result);
                })
                .expect("alert should present");
            });
        });
        cx.run_until_parked();

        assert!(cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert!(cx.debug_bounds("modal-surface-1").is_some());
    }

    #[gpui::test]
    fn alert_suppression_is_keyboard_reachable_and_returned(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                let outcome = root.outcome.clone();
                root.presentation = Some(
                    Alert::new(
                        ModalId::new("suppression-alert"),
                        "Suppression alert",
                        "Save Changes",
                        "Choose whether to save the current changes.",
                        vec![
                            ModalAction::new("save", "Save", ModalActionRole::Affirmative, "save")
                                .default_action(true),
                        ],
                    )
                    .suppression(AlertSuppression::new("Do not ask again", false))
                    .present(window, cx, move |result, _| {
                        *outcome.borrow_mut() = Some(result);
                    })
                    .expect("suppression alert should present"),
                );
            });
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();
        press_space(cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(
            matches!(
                outcome.borrow().as_ref(),
                Some(AlertOutcome::Activated {
                    action_id: "save",
                    suppression_selected: Some(true),
                    ..
                })
            ),
            "unexpected outcome: {:?}",
            outcome.borrow().as_ref()
        );
    }

    #[gpui::test]
    fn stale_progress_handle_clone_cannot_overwrite_a_newer_update(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::<()>::new(
                    ModalId::new("stale-progress-update"),
                    "Updating files",
                    "Updating Files",
                    "Starting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::programmatic_only(Duration::from_secs(30)),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| super::super::ProgressCancelDecision::Deny,
                    |_, _| {},
                )
                .expect("progress should present")
            })
        });
        let stale = handle.clone();
        cx.update(|window, cx| {
            handle
                .update(ProgressDialogUpdate::new().status("Newer"), window, cx)
                .expect("first update should succeed");
        });

        let result = cx.update(|window, cx| {
            stale.update(ProgressDialogUpdate::new().status("Older"), window, cx)
        });

        assert_eq!(
            result,
            Err(ModalUpdateError::StaleUpdate {
                attempted: 0,
                current: 1,
            })
        );
    }

    #[gpui::test]
    fn active_programmatic_progress_rejects_cancellation_and_keeps_escape_inert(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let progress_outcome = Rc::new(RefCell::new(None));
        let root_progress_outcome = progress_outcome.clone();
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::<()>::new(
                    ModalId::new("active-programmatic-progress"),
                    "Completing required work",
                    "Completing Work",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::programmatic_only(Duration::from_secs(30)),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Deny,
                    move |outcome, _| *root_progress_outcome.borrow_mut() = Some(outcome),
                )
                .expect("programmatic progress should present")
            })
        });
        cx.run_until_parked();

        let update_result = cx.update(|window, cx| {
            handle.update(
                ProgressDialogUpdate::new().cancellation_enabled(true),
                window,
                cx,
            )
        });
        cx.run_until_parked();
        let before_escape = cx.update(|window, cx| {
            (
                super::super::core::active_progress_for_test(window, cx),
                super::super::core::active_progress_presentation_facts_for_test(window, cx),
                super::super::window_modal_is_open(window, cx),
            )
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(
            update_result == Err(ModalUpdateError::CancellationNotSupported)
                && before_escape.0.is_some_and(|(_, progress, generation)| {
                    !progress.cancellation_capable
                        && !progress.cancellation_enabled
                        && generation == 0
                })
                && before_escape.1 == Some((false, 0))
                && before_escape.2
                && cx.debug_bounds("modal-action-cancel").is_none()
                && progress_outcome.borrow().is_none()
                && cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );
    }

    #[gpui::test]
    fn programmatic_progress_completion_restores_ordinary_predecessor_focus(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let invoker = root.read_with(cx, |root, _| root.invoker.clone());
        let handle = cx.update(|window, cx| {
            window.activate_window();
            invoker.focus(window);
            root.update(cx, |_, cx| {
                ProgressDialog::<()>::new(
                    ModalId::new("predecessor-progress"),
                    "Restoring predecessor focus",
                    "Working",
                    "Waiting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::programmatic_only(Duration::from_secs(30)),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Deny,
                    |_, _| {},
                )
                .expect("programmatic ProgressDialog should present")
            })
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            handle
                .complete(window, cx)
                .expect("programmatic ProgressDialog should complete");
        });
        cx.run_until_parked();

        assert!(cx.update(|window, _| invoker.is_focused(window)));
    }

    #[gpui::test]
    fn inactive_modal_close_defers_exact_predecessor_restoration_until_reactivation(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(DeferredRestorationFixture::new);
        let (invoker, invoker_focuses, unrelated_focuses) = root.read_with(cx, |root, _| {
            (
                root.invoker.clone().expect("rendered invoker should exist"),
                root.invoker_focuses.clone(),
                root.unrelated_focuses.clone(),
            )
        });
        let presentation = cx.update(|window, cx| {
            window.activate_window();
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
            root.read(cx)
                .presentation
                .clone()
                .expect("Alert presentation should be retained")
        });
        cx.run_until_parked();
        let focus_count_before_close = invoker_focuses.get();

        cx.deactivate_window();
        cx.update(|window, cx| {
            presentation
                .dismiss(window, cx)
                .expect("inactive Alert should dismiss");
        });
        cx.run_until_parked();
        let focus_count_while_inactive = invoker_focuses.get();
        let invoker_focused_while_inactive = cx.update(|window, _| invoker.is_focused(window));

        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        assert!(
            focus_count_while_inactive == focus_count_before_close
                && invoker_focuses.get() == focus_count_before_close + 1
                && unrelated_focuses.get() == 0
                && !invoker_focused_while_inactive
                && cx.update(|window, _| invoker.is_focused(window)),
        );
    }

    #[gpui::test]
    fn removed_predecessor_is_not_restored_or_replaced_by_unrelated_focus(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(DeferredRestorationFixture::new);
        let (invoker, unrelated, unrelated_focuses) = root.read_with(cx, |root, _| {
            (
                root.invoker.clone().expect("rendered invoker should exist"),
                root.unrelated.clone(),
                root.unrelated_focuses.clone(),
            )
        });
        let removed_invoker = invoker.downgrade();
        let presentation = cx.update(|window, cx| {
            window.activate_window();
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
            root.read(cx)
                .presentation
                .clone()
                .expect("Alert presentation should be retained")
        });
        cx.run_until_parked();
        root.update(cx, |root, cx| {
            root.invoker = None;
            cx.notify();
        });
        cx.run_until_parked();

        cx.deactivate_window();
        cx.update(|window, cx| {
            presentation
                .dismiss(window, cx)
                .expect("inactive Alert should dismiss after invoker removal");
        });
        cx.run_until_parked();
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        assert!(
            removed_invoker.upgrade().is_some()
                && !cx.update(|window, _| invoker.is_focused(window))
                && unrelated_focuses.get() == 0
                && !cx.update(|window, _| unrelated.is_focused(window)),
        );
    }

    #[gpui::test]
    fn queued_programmatic_progress_rejects_cancellation_and_stays_inert_after_promotion(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let blocker = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("active Alert should be retained");
        let progress_outcome = Rc::new(RefCell::new(None));
        let root_progress_outcome = progress_outcome.clone();
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::<()>::new(
                    ModalId::new("queued-programmatic-progress"),
                    "Completing queued required work",
                    "Completing Queued Work",
                    "Waiting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::programmatic_only(Duration::from_secs(30)),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| ProgressCancelDecision::Deny,
                    move |outcome, _| *root_progress_outcome.borrow_mut() = Some(outcome),
                )
                .expect("programmatic progress should queue")
            })
        });
        let update_result = cx.update(|window, cx| {
            handle.update(
                ProgressDialogUpdate::new().cancellation_enabled(true),
                window,
                cx,
            )
        });
        cx.update(|window, cx| {
            blocker.dismiss(window, cx).expect("blocker should close");
        });
        cx.run_until_parked();
        let promoted = cx.update(|window, cx| {
            (
                super::super::core::active_progress_for_test(window, cx),
                super::super::core::active_progress_presentation_facts_for_test(window, cx),
            )
        });
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(
            (
                update_result,
                promoted.0.map(|(presentation, progress, generation)| (
                    presentation,
                    progress.cancellation_capable,
                    progress.cancellation_enabled,
                    generation,
                )),
                promoted.1,
                *progress_outcome.borrow(),
                cx.update(|window, cx| super::super::window_modal_is_open(window, cx)),
            ),
            (
                Err(ModalUpdateError::CancellationNotSupported),
                Some((handle.presentation_id(), false, false, 0)),
                Some((false, 0)),
                None,
                true,
            )
        );
    }

    #[gpui::test]
    fn queued_initially_disabled_progress_enables_and_routes_escape_after_promotion(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let blocker = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("active Alert should be retained");
        let requests = Rc::new(Cell::new(0));
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let root_outcomes = outcomes.clone();
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("queued-initially-disabled-progress"),
                    "Queued cancellation starts unavailable",
                    "Queued Work",
                    "Waiting",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(
                        ModalAction::new(
                            (),
                            "Cancel",
                            ModalActionRole::Cancel,
                            "queued-initially-disabled-cancel",
                        )
                        .enabled(false),
                    ),
                )
                .present(
                    window,
                    cx,
                    move |_, _, _| {
                        root_requests.set(root_requests.get() + 1);
                        ProgressCancelDecision::Allow
                    },
                    move |outcome, _| root_outcomes.borrow_mut().push(outcome),
                )
                .expect("initially disabled ProgressDialog should queue")
            })
        });
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new()
                        .status("Enabled while queued")
                        .cancellation_enabled(true),
                    window,
                    cx,
                )
                .expect("queued cancellation should enable");
            blocker.dismiss(window, cx).expect("blocker should close");
        });
        cx.run_until_parked();
        let promoted = cx.update(|window, cx| {
            (
                super::super::core::active_progress_for_test(window, cx)
                    .expect("updated ProgressDialog should promote"),
                super::super::core::active_progress_presentation_facts_for_test(window, cx),
            )
        });

        cx.simulate_keystrokes("escape escape");
        cx.run_until_parked();

        assert!(
            promoted.0.0 == handle.presentation_id()
                && promoted.0.1.status.as_ref() == "Enabled while queued"
                && promoted.0.1.cancellation_capable
                && promoted.0.1.cancellation_enabled
                && promoted.0.2 == 1
                && promoted.1 == Some((true, 1))
                && requests.get() == 1
                && outcomes.borrow().as_slice()
                    == [ProgressDialogOutcome::Cancelled {
                        source: ModalActivationSource::Escape,
                    }]
                && !cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );
    }

    #[gpui::test]
    fn queued_progress_update_should_transfer_runtime_and_generation_on_promotion(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let blocker = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("active Alert should be retained");
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "queued-progress-update",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });
        let stale = handle.clone();
        let determinate = DeterminateProgress::new(0.625).expect("progress should normalize");
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new()
                        .status("Updated while queued")
                        .detail(Some("Retained detail"))
                        .progress(ProgressState::Determinate(determinate))
                        .cancellation_enabled(false),
                    window,
                    cx,
                )
                .expect("queued update should succeed");
        });
        let stale_result = cx.update(|window, cx| {
            stale.update(ProgressDialogUpdate::new().status("Obsolete"), window, cx)
        });
        cx.update(|window, cx| {
            blocker.dismiss(window, cx).expect("blocker should close");
        });
        cx.run_until_parked();
        let promoted = cx.update(|window, cx| {
            super::super::core::active_progress_for_test(window, cx)
                .expect("updated ProgressDialog should promote")
        });
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().status("Updated after promotion"),
                    window,
                    cx,
                )
                .expect("promoted handle should retain its update generation");
        });

        assert!(
            stale_result
                == Err(ModalUpdateError::StaleUpdate {
                    attempted: 0,
                    current: 1,
                })
                && promoted.0 == handle.presentation_id()
                && promoted.1.status.as_ref() == "Updated while queued"
                && promoted.1.detail.as_ref().map(|detail| detail.as_ref())
                    == Some("Retained detail")
                && promoted.1.progress == ProgressState::Determinate(determinate)
                && !promoted.1.cancellation_enabled
                && promoted.2 == 1
                && lifecycle.borrow().as_slice()
                    == [ModalLifecycleEvent::Opened(handle.presentation_id())]
                && outcomes.borrow().is_empty()
        );
    }

    #[gpui::test]
    fn queued_progress_complete_should_deliver_completed_and_only_closed_once(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "queued-progress-complete",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });
        cx.update(|window, cx| {
            handle
                .complete(window, cx)
                .expect("queued completion should succeed");
        });
        let duplicate = cx.update(|window, cx| handle.fail(window, cx));

        assert_eq!(
            (
                outcomes.borrow().clone(),
                lifecycle.borrow().clone(),
                duplicate,
            ),
            (
                vec![ProgressDialogOutcome::Completed],
                vec![ModalLifecycleEvent::Closed(
                    handle.presentation_id(),
                    super::super::ModalCloseReason::Programmatic,
                )],
                Err(ModalTerminalOutcomeError::AlreadyDelivered),
            )
        );
    }

    #[gpui::test]
    fn queued_progress_fail_should_deliver_failed_and_only_closed(cx: &mut TestAppContext) {
        let (root, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "queued-progress-fail",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });
        cx.update(|window, cx| {
            handle
                .fail(window, cx)
                .expect("queued failure should succeed");
        });

        assert_eq!(
            (outcomes.borrow().clone(), lifecycle.borrow().clone(),),
            (
                vec![ProgressDialogOutcome::Failed],
                vec![ModalLifecycleEvent::Closed(
                    handle.presentation_id(),
                    super::super::ModalCloseReason::Programmatic,
                )],
            )
        );
    }

    #[gpui::test]
    fn queued_progress_dismiss_should_deliver_dismissal_and_only_closed(cx: &mut TestAppContext) {
        let (root, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "queued-progress-dismiss",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });
        cx.update(|window, cx| {
            handle
                .dismiss(window, cx)
                .expect("queued dismissal should succeed");
        });

        assert_eq!(
            (outcomes.borrow().clone(), lifecycle.borrow().clone(),),
            (
                vec![ProgressDialogOutcome::ProgrammaticDismissal],
                vec![ModalLifecycleEvent::Closed(
                    handle.presentation_id(),
                    super::super::ModalCloseReason::Programmatic,
                )],
            )
        );
    }

    struct ActiveAlertCaller;

    impl ActiveAlertCaller {
        fn present(
            &mut self,
            outcome: Rc<RefCell<Option<AlertOutcome<&'static str>>>>,
            window: &Window,
            cx: &mut Context<Self>,
        ) {
            Alert::new(
                ModalId::new("removed-active-alert-caller"),
                "Caller-owned alert",
                "Caller-owned Alert",
                "This alert closes when its caller is removed.",
                vec![
                    ModalAction::new(
                        "save",
                        "Save",
                        ModalActionRole::Affirmative,
                        "removed-active-alert-save",
                    )
                    .default_action(true),
                    ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "removed-active-alert-cancel",
                    ),
                ],
            )
            .suppression(AlertSuppression::new("Do not ask again", false))
            .present(window, cx, move |result, _| {
                *outcome.borrow_mut() = Some(result);
            })
            .expect("caller-owned Alert should present");
        }
    }

    #[gpui::test]
    fn caller_removal_cancels_active_suppression_press(cx: &mut TestAppContext) {
        let (root, _, _, cx) = alert_window(cx);
        let first = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("fixture alert should have a handle");
        cx.update(|window, cx| {
            first
                .dismiss(window, cx)
                .expect("fixture alert should dismiss");
        });
        cx.run_until_parked();

        let outcome = Rc::new(RefCell::new(None));
        let caller_outcome = outcome.clone();
        let caller = cx.update(|window, cx| {
            let caller = cx.new(|_| ActiveAlertCaller);
            caller.update(cx, |caller, cx| caller.present(caller_outcome, window, cx));
            caller
        });
        cx.run_until_parked();
        let suppression = cx
            .debug_bounds("modal-alert-suppression")
            .expect("caller-owned suppression should render");
        cx.simulate_mouse_down(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );

        drop(caller);
        cx.update(|_, _| {});
        cx.run_until_parked();
        cx.simulate_mouse_up(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Dismissed {
                reason: super::super::ModalCloseReason::OwnerRemoved,
                suppression_selected: Some(false),
            })
        ));
    }

    struct QueuedProgressCaller;

    impl QueuedProgressCaller {
        fn present(
            &mut self,
            outcomes: Rc<RefCell<Vec<ProgressDialogOutcome>>>,
            lifecycle: Rc<RefCell<Vec<ModalLifecycleEvent>>>,
            window: &Window,
            cx: &mut Context<Self>,
        ) -> super::super::ProgressDialogHandle {
            ProgressDialog::<()>::new(
                ModalId::new("removed-caller-progress"),
                "Caller-owned queued operation",
                "Queued Operation",
                "Waiting",
                ProgressState::Indeterminate,
                ProgressCancellation::programmatic_only(Duration::from_secs(30)),
            )
            .present_with_lifecycle(
                window,
                cx,
                |_, _, _| super::super::ProgressCancelDecision::Deny,
                move |outcome, _| outcomes.borrow_mut().push(outcome),
                move |event, _| lifecycle.borrow_mut().push(*event),
            )
            .expect("caller-owned ProgressDialog should queue")
        }
    }

    #[gpui::test]
    fn caller_removal_should_resolve_exact_queued_progress_without_opening(
        cx: &mut TestAppContext,
    ) {
        let (_, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let (caller, handle) = cx.update(|window, cx| {
            let caller = cx.new(|_| QueuedProgressCaller);
            let handle = caller.update(cx, |caller, cx| {
                caller.present(outcomes.clone(), lifecycle.clone(), window, cx)
            });
            (caller, handle)
        });

        drop(caller);
        cx.update(|_, _| {});
        cx.run_until_parked();
        let terminal = cx.update(|window, cx| handle.complete(window, cx));

        assert_eq!(
            (
                outcomes.borrow().clone(),
                lifecycle.borrow().clone(),
                terminal,
            ),
            (
                vec![ProgressDialogOutcome::OwnerRemoved],
                vec![ModalLifecycleEvent::Closed(
                    handle.presentation_id(),
                    super::super::ModalCloseReason::OwnerRemoved,
                )],
                Err(ModalTerminalOutcomeError::OwnerRemoved),
            )
        );
    }

    struct QueuedDialogCaller;

    impl QueuedDialogCaller {
        fn present(
            &mut self,
            outcomes: Rc<RefCell<Vec<DialogOutcome<&'static str>>>>,
            window: &Window,
            cx: &mut Context<Self>,
        ) -> super::super::DialogCompletion {
            Dialog::new(
                ModalId::new("removed-caller-dialog"),
                "Caller-owned queued Dialog",
                "Queued Dialog",
                vec![
                    ModalAction::new(
                        "save",
                        "Save",
                        ModalActionRole::Affirmative,
                        "removed-dialog-save",
                    ),
                    ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "removed-dialog-cancel",
                    ),
                ],
                DialogInitialFocus::Action("save"),
            )
            .present(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Pending,
                move |outcome, _| outcomes.borrow_mut().push(outcome),
            )
            .expect("caller-owned Dialog should queue")
        }
    }

    #[gpui::test]
    fn caller_removal_resolves_queued_dialog_completion_exactly_once(cx: &mut TestAppContext) {
        let (_, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let (caller, completion) = cx.update(|window, cx| {
            let caller = cx.new(|_| QueuedDialogCaller);
            let completion = caller.update(cx, |caller, cx| {
                caller.present(outcomes.clone(), window, cx)
            });
            (caller, completion)
        });

        drop(caller);
        cx.update(|_, _| {});
        cx.run_until_parked();
        let terminal = cx.update(|window, cx| completion.complete(window, None, cx));

        assert_eq!(
            (outcomes.borrow().clone(), terminal,),
            (
                vec![DialogOutcome::Dismissed(
                    super::super::ModalCloseReason::OwnerRemoved,
                )],
                Err(ModalTerminalOutcomeError::OwnerRemoved),
            )
        );
    }

    #[gpui::test]
    fn operating_system_window_removal_should_resolve_queued_progress_without_opening(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let handle = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "removed-window-progress",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });

        cx.update(|window, _| window.remove_window());
        cx.run_until_parked();

        assert_eq!(
            (outcomes.borrow().clone(), lifecycle.borrow().clone(),),
            (
                vec![ProgressDialogOutcome::OwnerRemoved],
                vec![ModalLifecycleEvent::Closed(
                    handle.presentation_id(),
                    super::super::ModalCloseReason::OwnerRemoved,
                )],
            )
        );
    }

    #[gpui::test]
    fn queued_progress_terminal_removal_should_preserve_surrounding_fifo_order(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let blocker = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("active Alert should be retained");
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let progress = cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.queue_progress(
                    "fifo-progress",
                    outcomes.clone(),
                    lifecycle.clone(),
                    window,
                    cx,
                )
            })
        });
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        let following = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("following Alert should queue");

        cx.update(|window, cx| {
            progress
                .complete(window, cx)
                .expect("middle queued progress should complete");
            blocker.dismiss(window, cx).expect("blocker should close");
        });
        cx.run_until_parked();
        let current = cx.update(|window, cx| {
            super::super::core::current_modal_parent(window, cx).map(|parent| parent.presentation)
        });

        assert!(
            current == Some(following.presentation_id())
                && outcomes.borrow().as_slice() == [ProgressDialogOutcome::Completed]
                && lifecycle.borrow().as_slice()
                    == [ModalLifecycleEvent::Closed(
                        progress.presentation_id(),
                        super::super::ModalCloseReason::Programmatic,
                    )]
        );
    }

    struct PendingDialogFixture {
        requests: Rc<RefCell<Vec<&'static str>>>,
        completions: Rc<RefCell<Vec<(&'static str, DialogPendingCompletion)>>>,
        outcomes: Rc<RefCell<Vec<DialogOutcome<&'static str>>>>,
        presentation: Option<super::super::DialogCompletion>,
    }

    impl PendingDialogFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let requests = self.requests.clone();
            let completions = self.completions.clone();
            let outcomes = self.outcomes.clone();
            self.presentation = Some(
                Dialog::new(
                    ModalId::new("pending-dialog-fixture"),
                    "Save changes",
                    "Save Changes",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "pending-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "pending-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("save"),
                )
                .present(
                    window,
                    cx,
                    move |request, completion, _| {
                        let action = *request.action_id();
                        requests.borrow_mut().push(action);
                        completions.borrow_mut().push((action, completion));
                        DialogCloseDecision::Pending
                    },
                    move |outcome, _| outcomes.borrow_mut().push(outcome),
                )
                .expect("pending Dialog should present"),
            );
        }
    }

    impl Render for PendingDialogFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    type PendingDialogWindow<'a> = (
        Rc<RefCell<Vec<&'static str>>>,
        Rc<RefCell<Vec<(&'static str, DialogPendingCompletion)>>>,
        Rc<RefCell<Vec<DialogOutcome<&'static str>>>>,
        &'a mut VisualTestContext,
    );

    fn pending_dialog_window(cx: &mut TestAppContext) -> PendingDialogWindow<'_> {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let completions = Rc::new(RefCell::new(Vec::new()));
        let outcomes = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let root_completions = completions.clone();
        let root_outcomes = outcomes.clone();
        let (root, cx) = cx.add_window_view(move |_, _| PendingDialogFixture {
            requests: root_requests,
            completions: root_completions,
            outcomes: root_outcomes,
            presentation: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        (requests, completions, outcomes, cx)
    }

    fn request_primary_and_nested_cancel(cx: &mut VisualTestContext) {
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }

    #[gpui::test]
    fn pointer_duplicate_cancel_should_not_repeat_callback_or_advance_attempt_generation(
        cx: &mut TestAppContext,
    ) {
        let (requests, _, _, cx) = pending_dialog_window(cx);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let cancel = cx
            .debug_bounds("modal-action-pending-cancel")
            .expect("Cancel should render");
        cx.simulate_click(cancel.center(), Modifiers::default());
        cx.run_until_parked();
        let generation_before_duplicate = cx
            .update(|window, cx| super::super::core::close_attempt_generation_for_test(window, cx));

        cx.simulate_click(cancel.center(), Modifiers::default());
        cx.run_until_parked();
        let generation_after_duplicate = cx
            .update(|window, cx| super::super::core::close_attempt_generation_for_test(window, cx));

        assert_eq!(
            (
                requests
                    .borrow()
                    .iter()
                    .filter(|action| **action == "cancel")
                    .count(),
                generation_before_duplicate,
                generation_after_duplicate,
            ),
            (1, Some(2), Some(2))
        );
    }

    #[gpui::test]
    fn keyboard_duplicate_cancel_should_not_repeat_callback_or_advance_attempt_generation(
        cx: &mut TestAppContext,
    ) {
        let (requests, _, _, cx) = pending_dialog_window(cx);
        request_primary_and_nested_cancel(cx);
        let generation_before_duplicate = cx
            .update(|window, cx| super::super::core::close_attempt_generation_for_test(window, cx));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let generation_after_duplicate = cx
            .update(|window, cx| super::super::core::close_attempt_generation_for_test(window, cx));

        assert_eq!(
            (
                requests
                    .borrow()
                    .iter()
                    .filter(|action| **action == "cancel")
                    .count(),
                generation_before_duplicate,
                generation_after_duplicate,
            ),
            (1, Some(2), Some(2))
        );
    }

    #[gpui::test]
    fn nested_cancel_denial_should_restore_original_public_completion(cx: &mut TestAppContext) {
        let (_, completions, outcomes, cx) = pending_dialog_window(cx);
        request_primary_and_nested_cancel(cx);
        let primary = completions.borrow()[0].1.clone();
        let cancel = completions.borrow()[1].1.clone();

        cx.update(|window, cx| {
            cancel
                .deny(window, None, cx)
                .expect("nested Cancel denial should succeed");
            primary
                .allow(window, None, cx)
                .expect("original completion should remain authoritative");
        });
        cx.run_until_parked();

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [DialogOutcome::Completed {
                action_id: "save",
                source: ModalActivationSource::Return,
            }]
        ));
    }

    #[gpui::test]
    fn original_denial_should_preserve_nested_cancel_public_completion(cx: &mut TestAppContext) {
        let (_, completions, outcomes, cx) = pending_dialog_window(cx);
        request_primary_and_nested_cancel(cx);
        let primary = completions.borrow()[0].1.clone();
        let cancel = completions.borrow()[1].1.clone();

        cx.update(|window, cx| {
            primary
                .deny(window, None, cx)
                .expect("original denial should succeed");
            cancel
                .allow(window, None, cx)
                .expect("nested Cancel should remain authoritative");
        });
        cx.run_until_parked();

        assert!(matches!(
            outcomes.borrow().as_slice(),
            [DialogOutcome::Completed {
                action_id: "cancel",
                source: ModalActivationSource::Escape,
            }]
        ));
    }

    #[gpui::test]
    fn original_allow_should_win_public_completion_race(cx: &mut TestAppContext) {
        let (_, completions, outcomes, cx) = pending_dialog_window(cx);
        request_primary_and_nested_cancel(cx);
        let primary = completions.borrow()[0].1.clone();
        let cancel = completions.borrow()[1].1.clone();

        let stale = cx.update(|window, cx| {
            primary
                .allow(window, None, cx)
                .expect("original allow should close");
            cancel.allow(window, None, cx)
        });
        cx.run_until_parked();

        assert!(
            stale == Err(ModalTerminalOutcomeError::AlreadyDelivered)
                && matches!(
                    outcomes.borrow().as_slice(),
                    [DialogOutcome::Completed {
                        action_id: "save",
                        source: ModalActivationSource::Return,
                    }]
                )
        );
    }

    #[gpui::test]
    fn nested_cancel_allow_should_win_public_completion_race(cx: &mut TestAppContext) {
        let (_, completions, outcomes, cx) = pending_dialog_window(cx);
        request_primary_and_nested_cancel(cx);
        let primary = completions.borrow()[0].1.clone();
        let cancel = completions.borrow()[1].1.clone();

        let stale = cx.update(|window, cx| {
            cancel
                .allow(window, None, cx)
                .expect("nested Cancel allow should close");
            primary.allow(window, None, cx)
        });
        cx.run_until_parked();

        assert!(
            stale == Err(ModalTerminalOutcomeError::AlreadyDelivered)
                && matches!(
                    outcomes.borrow().as_slice(),
                    [DialogOutcome::Completed {
                        action_id: "cancel",
                        source: ModalActivationSource::Escape,
                    }]
                )
        );
    }

    #[gpui::test]
    fn public_dialog_replacement_settles_before_open_and_preserves_stale_authority_and_fifo(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let trace = Rc::new(RefCell::new(Vec::new()));
        let pending = Rc::new(RefCell::new(None));
        let result_count = Rc::new(Cell::new(0));
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let predecessor = cx.update(|window, cx| {
            let trace_for_action = trace.clone();
            let trace_for_result = trace.clone();
            let trace_for_lifecycle = trace.clone();
            let pending_for_action = pending.clone();
            let result_count = result_count.clone();
            root.update(cx, |_, cx| {
                Dialog::new(
                    ModalId::new("replace-pending-dialog"),
                    "Replace pending Dialog",
                    "Pending Dialog",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "replace-pending-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "replace-pending-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("save"),
                )
                .present_with_lifecycle(
                    window,
                    cx,
                    move |_, completion, _| {
                        trace_for_action.borrow_mut().push("predecessor:action");
                        *pending_for_action.borrow_mut() = Some(completion);
                        DialogCloseDecision::Pending
                    },
                    move |outcome, cx| {
                        assert_eq!(
                            outcome,
                            DialogOutcome::Dismissed(ModalCloseReason::Replaced)
                        );
                        trace_for_result.borrow_mut().push("predecessor:result");
                        result_count.set(result_count.get() + 1);
                        let marker = cx.new(|_| ());
                        drop(marker);
                    },
                    move |event, _| match event {
                        ModalLifecycleEvent::Opened(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:opened")
                        }
                        ModalLifecycleEvent::ActionRequested(_) => trace_for_lifecycle
                            .borrow_mut()
                            .push("predecessor:action-requested"),
                        ModalLifecycleEvent::Pending(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:pending")
                        }
                        ModalLifecycleEvent::Closing(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:closing")
                        }
                        ModalLifecycleEvent::Closed(_, ModalCloseReason::Replaced) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:closed")
                        }
                        ModalLifecycleEvent::Closed(_, _) => {}
                    },
                )
                .expect("predecessor Dialog should present")
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let queued_trace = trace.clone();
        let queued = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                Dialog::new(
                    ModalId::new("preserved-queued-dialog"),
                    "Preserved queued Dialog",
                    "Queued Dialog",
                    vec![
                        ModalAction::new(
                            "continue",
                            "Continue",
                            ModalActionRole::Affirmative,
                            "preserved-queued-continue",
                        ),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "preserved-queued-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("continue"),
                )
                .present_with_lifecycle(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Pending,
                    |_, _| {},
                    move |event, _| {
                        if matches!(event, ModalLifecycleEvent::Opened(_)) {
                            queued_trace.borrow_mut().push("queued:opened");
                        }
                    },
                )
                .expect("Dialog should wait behind predecessor")
            })
        });

        let replacement_trace = trace.clone();
        let replacement = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                Dialog::new(
                    ModalId::new("replacement-dialog"),
                    "Replacement Dialog",
                    "Replacement Dialog",
                    vec![
                        ModalAction::new(
                            "finish",
                            "Finish",
                            ModalActionRole::Affirmative,
                            "replacement-finish",
                        ),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "replacement-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("finish"),
                )
                .replace_active(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Pending,
                    |_, _| {},
                    move |event, _| {
                        if matches!(event, ModalLifecycleEvent::Opened(_)) {
                            replacement_trace.borrow_mut().push("replacement:opened");
                        }
                    },
                )
                .expect("public Dialog replacement should succeed")
            })
        });
        cx.run_until_parked();

        let stale_pending = pending
            .borrow()
            .clone()
            .expect("pending authority should be retained");
        let (pending_result, dismissal_result, current) = cx.update(|window, cx| {
            (
                stale_pending.allow(window, None, cx),
                predecessor.dismiss(window, cx),
                super::super::core::current_modal_parent(window, cx)
                    .map(|parent| parent.presentation),
            )
        });
        assert_eq!(result_count.get(), 1);
        assert!(matches!(
            pending_result,
            Err(ModalTerminalOutcomeError::Stale(_))
        ));
        assert!(matches!(
            dismissal_result,
            Err(ModalDismissalError::Stale(_))
        ));
        assert_eq!(current, Some(replacement.presentation_id()));
        assert_eq!(
            trace.borrow().as_slice(),
            [
                "predecessor:opened",
                "predecessor:action-requested",
                "predecessor:action",
                "predecessor:pending",
                "predecessor:closing",
                "predecessor:result",
                "predecessor:closed",
                "replacement:opened",
            ]
        );

        cx.update(|window, cx| {
            replacement
                .complete(window, None, cx)
                .expect("replacement should complete");
        });
        cx.run_until_parked();
        assert_eq!(trace.borrow().last(), Some(&"queued:opened"));
        cx.update(|window, cx| {
            queued
                .complete(window, None, cx)
                .expect("preserved queued Dialog should complete");
        });
    }

    struct PendingProgressFixture {
        completion: Rc<RefCell<Option<ProgressCancellationCompletion>>>,
        outcome: Rc<RefCell<Option<ProgressDialogOutcome>>>,
        handle: Option<super::super::ProgressDialogHandle>,
    }

    impl PendingProgressFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let completion = self.completion.clone();
            let outcome = self.outcome.clone();
            self.handle = Some(
                ProgressDialog::new(
                    ModalId::new("pending-progress-fixture"),
                    "Cancelling operation",
                    "Cancelling Operation",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "pending-progress-cancel",
                    )),
                )
                .present(
                    window,
                    cx,
                    move |_, authority, _| {
                        *completion.borrow_mut() = Some(authority);
                        ProgressCancelDecision::Pending
                    },
                    move |result, _| *outcome.borrow_mut() = Some(result),
                )
                .expect("pending ProgressDialog should present"),
            );
        }
    }

    impl Render for PendingProgressFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    #[gpui::test]
    fn disabling_reenabled_progress_cancellation_invalidates_attempt_and_owned_press(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let requests = Rc::new(Cell::new(0));
        let completion = Rc::new(RefCell::new(None));
        let root_requests = requests.clone();
        let root_completion = completion.clone();
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("invalidate-reenabled-progress"),
                    "Cancellation availability changes",
                    "Updating Files",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(
                        ModalAction::new(
                            (),
                            "Cancel",
                            ModalActionRole::Cancel,
                            "invalidate-reenabled-cancel",
                        )
                        .enabled(false),
                    ),
                )
                .present(
                    window,
                    cx,
                    move |_, authority, _| {
                        root_requests.set(root_requests.get() + 1);
                        *root_completion.borrow_mut() = Some(authority);
                        ProgressCancelDecision::Pending
                    },
                    |_, _| {},
                )
                .expect("initially disabled cancellable ProgressDialog should present")
            })
        });
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(true),
                    window,
                    cx,
                )
                .expect("cancellation should enable");
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("escape escape");
        cx.run_until_parked();
        let authority = completion
            .borrow()
            .clone()
            .expect("enabled Escape should retain cancellation authority");
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(false),
                    window,
                    cx,
                )
                .expect("cancellation should disable and invalidate the attempt");
        });
        let stale_attempt = cx.update(|window, cx| authority.allow(window, cx));

        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(true),
                    window,
                    cx,
                )
                .expect("cancellation should re-enable");
        });
        cx.run_until_parked();
        let cancel = cx
            .debug_bounds("modal-action-invalidate-reenabled-cancel")
            .expect("re-enabled Cancel action should render");
        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(false),
                    window,
                    cx,
                )
                .expect("cancellation should disable during the owned press");
        });
        let idle = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            requests.get() == 1
                && matches!(stale_attempt, Err(ModalTerminalOutcomeError::Stale(_)))
                && idle
                && cx.update(|window, cx| {
                    super::super::core::modal_button_controls_are_idle_for_test(window, cx)
                        && super::super::window_modal_is_open(window, cx)
                })
        );
    }

    #[gpui::test]
    fn delayed_progress_cancel_allow_should_retain_escape_source(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let completion = Rc::new(RefCell::new(None));
        let outcome = Rc::new(RefCell::new(None));
        let root_completion = completion.clone();
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, _| PendingProgressFixture {
            completion: root_completion,
            outcome: root_outcome,
            handle: None,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let authority = completion
            .borrow()
            .clone()
            .expect("Escape cancellation should retain completion authority");
        cx.update(|window, cx| {
            authority
                .allow(window, cx)
                .expect("delayed cancellation should allow");
        });
        cx.run_until_parked();

        assert_eq!(
            *outcome.borrow(),
            Some(ProgressDialogOutcome::Cancelled {
                source: ModalActivationSource::Escape,
            })
        );
    }

    #[gpui::test]
    fn public_progress_replacement_rejects_cancellation_terminal_and_update_authority(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let trace = Rc::new(RefCell::new(Vec::new()));
        let cancellation = Rc::new(RefCell::new(None));
        let successor_cancel_requests = Rc::new(Cell::new(0));
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let predecessor = cx.update(|window, cx| {
            let trace_for_cancel = trace.clone();
            let trace_for_result = trace.clone();
            let trace_for_lifecycle = trace.clone();
            let cancellation = cancellation.clone();
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("replace-pending-progress"),
                    "Replace pending progress",
                    "Pending Progress",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "replace-pending-progress-cancel",
                    )),
                )
                .present_with_lifecycle(
                    window,
                    cx,
                    move |_, completion, _| {
                        trace_for_cancel.borrow_mut().push("predecessor:cancel");
                        *cancellation.borrow_mut() = Some(completion);
                        ProgressCancelDecision::Pending
                    },
                    move |outcome, _| {
                        assert_eq!(outcome, ProgressDialogOutcome::Replaced);
                        trace_for_result.borrow_mut().push("predecessor:result");
                    },
                    move |event, _| match event {
                        ModalLifecycleEvent::Opened(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:opened")
                        }
                        ModalLifecycleEvent::ActionRequested(_) => trace_for_lifecycle
                            .borrow_mut()
                            .push("predecessor:action-requested"),
                        ModalLifecycleEvent::Pending(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:pending")
                        }
                        ModalLifecycleEvent::Closing(_) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:closing")
                        }
                        ModalLifecycleEvent::Closed(_, ModalCloseReason::Replaced) => {
                            trace_for_lifecycle.borrow_mut().push("predecessor:closed")
                        }
                        ModalLifecycleEvent::Closed(_, _) => {}
                    },
                )
                .expect("predecessor ProgressDialog should present")
            })
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        let stale_update = predecessor.clone();
        let cancel_bounds = cx
            .debug_bounds("modal-action-replace-pending-progress-cancel")
            .expect("predecessor Cancel should render");
        cx.simulate_mouse_down(
            cancel_bounds.center(),
            MouseButton::Left,
            Modifiers::default(),
        );

        let replacement_trace = trace.clone();
        let successor_cancel_requests_for_callback = successor_cancel_requests.clone();
        let replacement = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("replacement-progress"),
                    "Replacement progress",
                    "Replacement Progress",
                    "Continuing",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "replacement-progress-cancel",
                    )),
                )
                .replace_active(
                    window,
                    cx,
                    move |_, _, _| {
                        successor_cancel_requests_for_callback
                            .set(successor_cancel_requests_for_callback.get() + 1);
                        ProgressCancelDecision::Deny
                    },
                    |_, _| {},
                    move |event, _| {
                        if matches!(event, ModalLifecycleEvent::Opened(_)) {
                            replacement_trace.borrow_mut().push("replacement:opened");
                        }
                    },
                )
                .expect("public ProgressDialog replacement should succeed")
            })
        });
        let controls_are_idle = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.run_until_parked();
        let replacement_cancel = cx
            .debug_bounds("modal-action-replacement-progress-cancel")
            .expect("replacement Cancel should render");
        cx.simulate_mouse_up(
            replacement_cancel.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();

        let stale_cancellation = cancellation
            .borrow()
            .clone()
            .expect("pending cancellation authority should be retained");
        let (cancel_result, completion_result, dismissal_result, update_result, current) = cx
            .update(|window, cx| {
                (
                    stale_cancellation.allow(window, cx),
                    predecessor.complete(window, cx),
                    predecessor.dismiss(window, cx),
                    stale_update.update(
                        ProgressDialogUpdate::new().status("Stale status"),
                        window,
                        cx,
                    ),
                    super::super::core::current_modal_parent(window, cx)
                        .map(|parent| parent.presentation),
                )
            });

        assert!(controls_are_idle);
        assert_eq!(successor_cancel_requests.get(), 0);
        assert!(matches!(
            cancel_result,
            Err(ModalTerminalOutcomeError::Stale(_))
        ));
        assert!(matches!(
            completion_result,
            Err(ModalTerminalOutcomeError::Stale(_))
        ));
        assert!(matches!(
            dismissal_result,
            Err(ModalTerminalOutcomeError::Stale(_))
        ));
        assert!(matches!(update_result, Err(ModalUpdateError::Stale(_))));
        assert_eq!(current, Some(replacement.presentation_id()));
        assert_eq!(
            trace.borrow().as_slice(),
            [
                "predecessor:opened",
                "predecessor:action-requested",
                "predecessor:cancel",
                "predecessor:pending",
                "predecessor:closing",
                "predecessor:result",
                "predecessor:closed",
                "replacement:opened",
            ]
        );

        cx.update(|window, cx| {
            replacement
                .complete(window, cx)
                .expect("replacement ProgressDialog should complete");
        });
    }

    #[gpui::test]
    fn modal_escape_activates_only_the_safe_cancel_action(cx: &mut TestAppContext) {
        let (root, _, outcome, cx) = alert_window(cx);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(
            matches!(
                outcome.borrow().as_ref(),
                Some(AlertOutcome::Activated {
                    action_id: "cancel",
                    source: ModalActivationSource::Escape,
                    ..
                })
            ),
            "unexpected outcome: {:?}",
            outcome.borrow().as_ref()
        );
        let invoker = root.read_with(cx, |root, _| root.invoker.clone());
        assert!(cx.update(|window, _| invoker.is_focused(window)));
    }

    #[gpui::test]
    fn modal_return_activates_the_explicit_default(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Activated {
                action_id: "save",
                source: ModalActivationSource::Return,
                ..
            })
        ));
    }

    #[gpui::test]
    fn modal_command_period_activates_the_safe_cancel(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);

        cx.simulate_keystrokes("cmd-.");
        cx.run_until_parked();

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Activated {
                action_id: "cancel",
                source: ModalActivationSource::CommandPeriod,
                ..
            })
        ));
    }

    #[gpui::test]
    fn modal_tab_and_shift_tab_wrap_inside_the_action_ring(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);

        cx.simulate_keystrokes("tab shift-tab");
        cx.simulate_event(KeyDownEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent {
            keystroke: Keystroke::parse("space").unwrap_or_default(),
        });
        cx.run_until_parked();

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Activated {
                action_id: "save",
                source: ModalActivationSource::Space,
                ..
            })
        ));
    }

    enum DialogBodyParentOperation {
        Complete(super::super::DialogCompletion),
        Replace(gpui::WeakEntity<DialogBodyButtonFixture>),
    }

    struct DialogBodyButton {
        activations: Rc<Cell<usize>>,
        successor: Rc<RefCell<Option<super::super::DialogCompletion>>>,
        operation: Rc<RefCell<Option<DialogBodyParentOperation>>>,
        controls_are_idle: Rc<Cell<Option<bool>>>,
    }

    impl Render for DialogBodyButton {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let activations = self.activations.clone();
            let successor = self.successor.clone();
            let operation = self.operation.clone();
            let controls_are_idle = self.controls_are_idle.clone();
            let parent_release = canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                        if !phase.capture() || event.button != MouseButton::Left {
                            return;
                        }
                        if let Some(operation) = operation.borrow_mut().take() {
                            match operation {
                                DialogBodyParentOperation::Complete(completion) => completion
                                    .complete(window, None, cx)
                                    .expect("active Dialog should complete before body release"),
                                DialogBodyParentOperation::Replace(root) => {
                                    root.update(cx, |root, cx| root.replace_active(window, cx))
                                        .expect("Dialog fixture should survive active replacement");
                                }
                            }
                            controls_are_idle.set(Some(
                                super::super::core::modal_button_controls_are_idle_for_test(
                                    window, cx,
                                ),
                            ));
                        }
                    });
                },
            )
            .absolute()
            .inset_0();
            div().relative().child(parent_release).child(
                Button::new("dialog-body-button", "Body action")
                    .debug_selector("dialog-body-button")
                    .on_activate(move |_, window, cx| {
                        activations.set(activations.get() + 1);
                        let successor = successor.borrow().clone();
                        if let Some(successor) = successor {
                            successor
                                .complete(window, None, cx)
                                .expect("body callback should be able to complete the successor");
                        }
                    }),
            )
        }
    }

    struct DialogBodyButtonFixture {
        body: Entity<DialogBodyButton>,
        active: Option<super::super::DialogCompletion>,
        successor: Rc<RefCell<Option<super::super::DialogCompletion>>>,
        successor_outcome: Rc<RefCell<Option<DialogOutcome<&'static str>>>>,
    }

    impl DialogBodyButtonFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            self.active = Some(
                Dialog::new(
                    ModalId::new("dialog-body-button-predecessor"),
                    "Dialog body button predecessor",
                    "Body Button",
                    vec![ModalAction::new(
                        "cancel",
                        "Cancel",
                        ModalActionRole::Cancel,
                        "dialog-body-button-predecessor-cancel",
                    )],
                    DialogInitialFocus::Action("cancel"),
                )
                .body(self.body.clone())
                .present(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Deny {
                        first_invalid: None,
                    },
                    |_, _| {},
                )
                .expect("Dialog with public body Button should present"),
            );
        }

        fn replace_active(
            &mut self,
            window: &Window,
            cx: &mut Context<Self>,
        ) -> super::super::DialogCompletion {
            let outcome = self.successor_outcome.clone();
            let successor = Dialog::new(
                ModalId::new("dialog-body-button-successor"),
                "Dialog body button successor",
                "Successor",
                vec![ModalAction::new(
                    "cancel",
                    "Cancel",
                    ModalActionRole::Cancel,
                    "dialog-body-button-successor-cancel",
                )],
                DialogInitialFocus::Action("cancel"),
            )
            .body(self.body.clone())
            .replace_active(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Deny {
                    first_invalid: None,
                },
                move |result, _| *outcome.borrow_mut() = Some(result),
                |_, _| {},
            )
            .expect("successor Dialog should replace the active predecessor");
            *self.successor.borrow_mut() = Some(successor.clone());
            successor
        }
    }

    impl Render for DialogBodyButtonFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    type DialogBodyButtonWindow<'a> = (
        Entity<DialogBodyButtonFixture>,
        Rc<Cell<usize>>,
        Rc<RefCell<Option<DialogBodyParentOperation>>>,
        Rc<Cell<Option<bool>>>,
        Rc<RefCell<Option<DialogOutcome<&'static str>>>>,
        &'a mut VisualTestContext,
    );

    fn dialog_body_button_window(cx: &mut TestAppContext) -> DialogBodyButtonWindow<'_> {
        install_test_catalogs(cx);
        let activations = Rc::new(Cell::new(0));
        let successor = Rc::new(RefCell::new(None));
        let operation = Rc::new(RefCell::new(None));
        let controls_are_idle = Rc::new(Cell::new(None));
        let successor_outcome = Rc::new(RefCell::new(None));
        let root_activations = activations.clone();
        let root_successor = successor.clone();
        let root_operation = operation.clone();
        let root_controls_are_idle = controls_are_idle.clone();
        let root_successor_outcome = successor_outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| DialogBodyButtonFixture {
            body: cx.new(|_| DialogBodyButton {
                activations: root_activations,
                successor: root_successor.clone(),
                operation: root_operation,
                controls_are_idle: root_controls_are_idle,
            }),
            active: None,
            successor: root_successor,
            successor_outcome: root_successor_outcome,
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        (
            root,
            activations,
            operation,
            controls_are_idle,
            successor_outcome,
            cx,
        )
    }

    #[gpui::test]
    fn dialog_programmatic_completion_disarms_public_body_button_before_release(
        cx: &mut TestAppContext,
    ) {
        let (root, activations, operation, controls_are_idle, _, cx) =
            dialog_body_button_window(cx);
        let button = cx
            .debug_bounds("dialog-body-button")
            .expect("public Dialog body Button should render");
        cx.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());
        let completion = root
            .read_with(cx, |root, _| root.active.clone())
            .expect("active Dialog completion should be retained");

        *operation.borrow_mut() = Some(DialogBodyParentOperation::Complete(completion));
        cx.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            controls_are_idle.get() == Some(true)
                && activations.get() == 0
                && !cx.update(|window, cx| super::super::window_modal_is_open(window, cx)),
            "a release from the completed Dialog frame invoked its public body Button"
        );
    }

    #[gpui::test]
    fn dialog_active_replacement_disarms_public_body_button_before_release(
        cx: &mut TestAppContext,
    ) {
        let (root, activations, operation, controls_are_idle, successor_outcome, cx) =
            dialog_body_button_window(cx);
        let button = cx
            .debug_bounds("dialog-body-button")
            .expect("predecessor public Dialog body Button should render");
        cx.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());

        *operation.borrow_mut() = Some(DialogBodyParentOperation::Replace(root.downgrade()));
        cx.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        let successor = root
            .read_with(cx, |root, _| root.successor.borrow().clone())
            .expect("successor should replace the predecessor before body release");
        let current = cx.update(|window, cx| {
            super::super::core::current_modal_parent(window, cx).map(|parent| parent.presentation)
        });

        assert!(
            controls_are_idle.get() == Some(true)
                && activations.get() == 0
                && successor_outcome.borrow().is_none()
                && current == Some(successor.presentation_id()),
            "a predecessor body Button release invoked its callback or completed the successor"
        );
    }

    #[gpui::test]
    fn modal_owner_removal_disarms_and_resolves_the_active_alert_once(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("cancel action should render");
        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());

        let idle_after_removal =
            cx.update(|window, cx| super::super::core::retire_modal_owner_for_test(window, cx));
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_removal
                && matches!(
                    outcome.borrow().as_ref(),
                    Some(AlertOutcome::Dismissed {
                        reason: super::super::ModalCloseReason::OwnerRemoved,
                        ..
                    })
                )
        );
    }

    #[gpui::test]
    fn operating_system_window_owner_removal_cancels_suppression_press(cx: &mut TestAppContext) {
        let (_, _, outcome, cx) = alert_window(cx);
        let suppression = cx
            .debug_bounds("modal-alert-suppression")
            .expect("suppression should render");
        cx.simulate_mouse_down(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );

        let idle_after_removal =
            cx.update(|window, cx| super::super::core::retire_modal_owner_for_test(window, cx));
        cx.simulate_mouse_up(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();

        assert!(
            idle_after_removal
                && matches!(
                    outcome.borrow().as_ref(),
                    Some(AlertOutcome::Dismissed {
                        reason: super::super::ModalCloseReason::OwnerRemoved,
                        suppression_selected: Some(false),
                    })
                )
        );
    }

    #[gpui::test]
    fn modal_deactivation_retains_presentation_and_cancels_pressed_action(cx: &mut TestAppContext) {
        let (_, underlay, outcome, cx) = alert_window(cx);
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("cancel action should render");
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("modal surface should render");
        let outside = point(surface.left() - px(4.0), surface.top());

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.deactivate_window();
        let idle_after_deactivation = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_deactivation
                && outcome.borrow().is_none()
                && underlay.get() == 0
                && cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );
    }

    #[gpui::test]
    fn suppression_press_is_cancelled_across_operating_system_window_reactivation(
        cx: &mut TestAppContext,
    ) {
        let (_, _, outcome, cx) = alert_window(cx);
        let suppression = cx
            .debug_bounds("modal-alert-suppression")
            .expect("suppression should render");

        cx.simulate_mouse_down(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.deactivate_window();
        cx.update(|window, _| window.activate_window());
        cx.simulate_mouse_up(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(matches!(
            outcome.borrow().as_ref(),
            Some(AlertOutcome::Activated {
                action_id: "save",
                suppression_selected: Some(false),
                ..
            })
        ));
    }

    #[gpui::test]
    fn suppression_press_cannot_cross_queued_presentation_replacement(cx: &mut TestAppContext) {
        let (root, _, first_outcome, cx) = alert_window(cx);
        let second_outcome = Rc::new(RefCell::new(None));
        let queued_outcome = second_outcome.clone();
        cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                Alert::new(
                    ModalId::new("replacement-alert"),
                    "Replacement alert",
                    "Save Changes",
                    "Choose whether to save the current changes.",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "replacement-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "replacement-cancel",
                        ),
                    ],
                )
                .suppression(AlertSuppression::new("Do not ask again", false))
                .present(window, cx, move |result, _| {
                    *queued_outcome.borrow_mut() = Some(result);
                })
                .expect("replacement alert should queue");
            })
        });
        let suppression = cx
            .debug_bounds("modal-alert-suppression")
            .expect("suppression should render");
        cx.simulate_mouse_down(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        let first_handle = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("first alert handle should remain");
        cx.update(|window, cx| {
            first_handle
                .dismiss(window, cx)
                .expect("first alert should dismiss");
        });
        cx.run_until_parked();

        cx.simulate_mouse_up(
            suppression.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert!(
            matches!(
                first_outcome.borrow().as_ref(),
                Some(AlertOutcome::Dismissed {
                    suppression_selected: Some(false),
                    ..
                })
            ) && matches!(
                second_outcome.borrow().as_ref(),
                Some(AlertOutcome::Activated {
                    action_id: "save",
                    suppression_selected: Some(false),
                    ..
                })
            )
        );
    }

    #[gpui::test]
    fn modal_queue_should_keep_tooltip_suppression_continuous_until_the_final_close(
        cx: &mut TestAppContext,
    ) {
        let (root, _, _, cx) = alert_window(cx);
        let first = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("first alert should be retained");
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        let second = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("queued alert should be retained");

        cx.update(|window, cx| {
            first.dismiss(window, cx).expect("first alert should close");
        });
        cx.run_until_parked();
        assert!(cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) }));

        cx.update(|window, cx| {
            second
                .dismiss(window, cx)
                .expect("second alert should close");
        });
        cx.run_until_parked();

        assert!(
            !cx.update(|window, cx| { crate::tooltip::window_tooltips_suppressed(window, cx) })
        );
    }

    #[gpui::test]
    fn outside_release_disarms_modal_action_before_blocking_underlay_input(
        cx: &mut TestAppContext,
    ) {
        let (_, underlay, outcome, cx) = alert_window(cx);
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("cancel action should render");
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("modal surface should render");
        let outside = point(surface.left() - px(4.0), surface.top());

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
        let idle_after_outside = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_outside && outcome.borrow().is_none() && underlay.get() == 0,
            "outside blocking retained a stale modal action press: {:?}",
            outcome.borrow().as_ref()
        );
    }

    #[gpui::test]
    fn outside_matching_release_disarms_an_action_moved_off_inside_the_surface(
        cx: &mut TestAppContext,
    ) {
        let (_, underlay, outcome, cx) = alert_window(cx);
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("cancel action should render");
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("modal surface should render");
        let inside_surface = point(surface.left() + px(4.0), surface.top() + px(4.0));
        let outside = point(surface.left() - px(4.0), surface.top());

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(inside_surface, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
        let idle_after_release = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_release && outcome.borrow().is_none() && underlay.get() == 0,
            "outside matching release retained modal press ownership: {:?}",
            outcome.borrow().as_ref()
        );
    }

    #[gpui::test]
    fn lost_button_move_disarms_modal_action_before_outside_blocking(cx: &mut TestAppContext) {
        let (_, underlay, outcome, cx) = alert_window(cx);
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("cancel action should render");
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("modal surface should render");
        let outside = point(surface.left() - px(4.0), surface.top());

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_event(MouseMoveEvent {
            position: outside,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        let idle_after_lost_button = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_lost_button && outcome.borrow().is_none() && underlay.get() == 0,
            "lost-button observation retained modal press ownership: {:?}",
            outcome.borrow().as_ref()
        );
    }

    #[gpui::test]
    fn modal_action_replacement_does_not_inherit_the_predecessor_press(cx: &mut TestAppContext) {
        let (root, underlay, outcome, cx) = alert_window(cx);
        let first = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("first presentation should be retained");
        let cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("first Cancel action should render");

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            first
                .dismiss(window, cx)
                .expect("first presentation should dismiss");
        });
        let idle_before_replacement = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let replacement_cancel = cx
            .debug_bounds("modal-action-cancel")
            .expect("replacement Cancel action should render");
        let replacement_surface = cx
            .debug_bounds("modal-surface-2")
            .expect("replacement surface should render");
        let outside = point(
            replacement_surface.left() - px(4.0),
            replacement_surface.top(),
        );
        cx.simulate_mouse_up(
            replacement_cancel.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(
            replacement_cancel.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.simulate_mouse_up(
            replacement_cancel.center(),
            MouseButton::Left,
            Modifiers::default(),
        );
        cx.run_until_parked();

        assert!(
            idle_before_replacement
                && matches!(
                    outcome.borrow().as_ref(),
                    Some(AlertOutcome::Dismissed {
                        reason: super::super::ModalCloseReason::Programmatic,
                        ..
                    })
                )
                && underlay.get() == 0
                && cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );
    }

    #[gpui::test]
    fn progress_action_disablement_disarms_before_the_next_render(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let requests = Rc::new(Cell::new(0));
        let root_requests = requests.clone();
        let (root, cx) = cx.add_window_view(|_, cx| AlertFixture {
            invoker: cx.focus_handle().tab_stop(true),
            underlay_activations: Rc::new(Cell::new(0)),
            outcome: Rc::new(RefCell::new(None)),
            presentation: None,
        });
        let handle = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                ProgressDialog::new(
                    ModalId::new("disable-progress-action"),
                    "Disable progress action",
                    "Disabling Action",
                    "Working",
                    ProgressState::Indeterminate,
                    ProgressCancellation::Cancellable(ModalAction::new(
                        (),
                        "Cancel",
                        ModalActionRole::Cancel,
                        "disable-cancel",
                    )),
                )
                .present(
                    window,
                    cx,
                    move |_, _, _| {
                        root_requests.set(root_requests.get() + 1);
                        ProgressCancelDecision::Deny
                    },
                    |_, _| {},
                )
                .expect("progress should present")
            })
        });
        cx.run_until_parked();
        let cancel = cx
            .debug_bounds("modal-action-disable-cancel")
            .expect("progress Cancel action should render");

        cx.simulate_mouse_down(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(false),
                    window,
                    cx,
                )
                .expect("cancellation should disable");
        });
        let idle_after_disablement = cx.update(|window, cx| {
            super::super::core::modal_button_controls_are_idle_for_test(window, cx)
        });
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        cx.update(|window, cx| {
            handle
                .update(
                    ProgressDialogUpdate::new().cancellation_enabled(true),
                    window,
                    cx,
                )
                .expect("cancellation should re-enable");
        });
        cx.run_until_parked();
        let cancel = cx
            .debug_bounds("modal-action-disable-cancel")
            .expect("re-enabled Cancel action should render");
        let surface = cx
            .debug_bounds("modal-surface-1")
            .expect("modal surface should render");
        let outside = point(surface.left() - px(4.0), surface.top());
        cx.simulate_mouse_down(outside, MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_move(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(cancel.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();

        assert!(
            idle_after_disablement
                && requests.get() == 0
                && cx.update(|window, cx| {
                    super::super::core::modal_button_controls_are_idle_for_test(window, cx)
                        && super::super::window_modal_is_open(window, cx)
                })
        );
    }

    #[gpui::test]
    fn modal_scrim_blocks_underlay_pointer_click_through(cx: &mut TestAppContext) {
        let (_, underlay, _, cx) = alert_window(cx);
        let underlay_bounds = cx
            .debug_bounds("modal-underlay-button")
            .expect("underlay should be painted");

        cx.simulate_click(underlay_bounds.center(), Modifiers::default());

        assert_eq!(underlay.get(), 0);
    }

    #[gpui::test]
    fn modal_scrim_blocks_underlay_move_wheel_and_keyboard_input(cx: &mut TestAppContext) {
        let (_, underlay, _, cx) = alert_window(cx);
        let underlay_bounds = cx
            .debug_bounds("modal-underlay-button")
            .expect("underlay should be painted");
        let outside = underlay_bounds.center();

        cx.simulate_event(MouseMoveEvent {
            position: outside,
            pressed_button: None,
            modifiers: Modifiers::default(),
        });
        cx.simulate_event(ScrollWheelEvent {
            position: outside,
            delta: ScrollDelta::Pixels(point(px(0.0), px(-20.0))),
            modifiers: Modifiers::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.simulate_keystrokes("a");
        cx.run_until_parked();

        assert_eq!(underlay.get(), 0);
    }

    struct DialogCompletionFocusFixture {
        invoker: FocusHandle,
        successor: FocusHandle,
        newer_owner: FocusHandle,
        show_successor: bool,
        completion: Option<super::super::DialogCompletion>,
        outcome: Rc<RefCell<Option<DialogOutcome<&'static str>>>>,
    }

    impl DialogCompletionFocusFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let outcome = self.outcome.clone();
            self.completion = Some(
                Dialog::new(
                    ModalId::new("programmatic-completion-dialog"),
                    "Programmatic completion focus",
                    "Complete Dialog",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "completion-save",
                        ),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "completion-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("save"),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Pending,
                    move |result, _| *outcome.borrow_mut() = Some(result),
                )
                .expect("Dialog should present"),
            );
        }

        fn queue_completion(
            &mut self,
            id: &'static str,
            window: &Window,
            cx: &mut Context<Self>,
        ) -> super::super::DialogCompletion {
            Dialog::new(
                ModalId::new(id),
                "Queued programmatic completion focus",
                "Queued Dialog",
                vec![
                    ModalAction::new("save", "Save", ModalActionRole::Affirmative, "queued-save"),
                    ModalAction::new("cancel", "Cancel", ModalActionRole::Cancel, "queued-cancel"),
                ],
                DialogInitialFocus::Action("save"),
            )
            .present(
                window,
                cx,
                |_, _, _| DialogCloseDecision::Pending,
                |_, _| {},
            )
            .expect("Dialog should queue")
        }
    }

    impl Render for DialogCompletionFocusFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .child(div().track_focus(&self.invoker))
                    .when(self.show_successor, |root| {
                        root.child(div().track_focus(&self.successor))
                    })
                    .child(div().track_focus(&self.newer_owner)),
            )
        }
    }

    fn dialog_completion_focus_window(
        cx: &mut TestAppContext,
    ) -> (Entity<DialogCompletionFocusFixture>, &mut VisualTestContext) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| DialogCompletionFocusFixture {
            invoker: cx.focus_handle().tab_stop(true),
            successor: cx.focus_handle().tab_stop(true),
            newer_owner: cx.focus_handle().tab_stop(true),
            show_successor: true,
            completion: None,
            outcome: Rc::new(RefCell::new(None)),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, cx)
    }

    #[gpui::test]
    fn dialog_programmatic_completion_focuses_a_live_successor(cx: &mut TestAppContext) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, successor) =
            root.read_with(cx, |root, _| (root.invoker.clone(), root.successor.clone()));
        cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        let completion = root
            .read_with(cx, |root, _| root.completion.clone())
            .expect("completion should be retained");

        cx.update(|window, cx| {
            completion
                .complete(window, Some(successor.clone()), cx)
                .expect("Dialog should complete");
        });
        cx.run_until_parked();

        assert!(cx.update(|window, _| successor.is_focused(window)));
        assert_eq!(
            root.read_with(cx, |root, _| root.outcome.borrow().clone()),
            Some(DialogOutcome::ProgrammaticallyCompleted)
        );
    }

    #[gpui::test]
    fn queued_dialog_successor_is_restored_after_the_active_predecessor_closes(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, successor) =
            root.read_with(cx, |root, _| (root.invoker.clone(), root.successor.clone()));
        let (active, queued) = cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| {
                root.present(window, cx);
                let queued = root.queue_completion("queued-successor", window, cx);
                let active = root
                    .completion
                    .clone()
                    .expect("active completion should exist");
                (active, queued)
            })
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            queued
                .complete(window, Some(successor.clone()), cx)
                .expect("queued Dialog should complete");
            active
                .complete(window, None, cx)
                .expect("active predecessor should complete");
        });
        cx.run_until_parked();

        assert!(cx.update(|window, _| successor.is_focused(window)));
    }

    #[gpui::test]
    fn queued_dialog_removed_successor_is_not_restored(cx: &mut TestAppContext) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, successor) =
            root.read_with(cx, |root, _| (root.invoker.clone(), root.successor.clone()));
        let (active, queued) = cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| {
                root.present(window, cx);
                let queued = root.queue_completion("queued-removed-successor", window, cx);
                let active = root
                    .completion
                    .clone()
                    .expect("active completion should exist");
                (active, queued)
            })
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            queued
                .complete(window, Some(successor.clone()), cx)
                .expect("queued Dialog should complete");
        });
        root.update(cx, |root, cx| {
            root.show_successor = false;
            cx.notify();
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            active
                .complete(window, None, cx)
                .expect("active predecessor should complete");
        });
        cx.run_until_parked();

        assert!(!cx.update(|window, _| successor.is_focused(window)));
    }

    #[gpui::test]
    fn later_active_dialog_successor_supersedes_a_queued_dialog_successor(cx: &mut TestAppContext) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, queued_successor, active_successor) = root.read_with(cx, |root, _| {
            (
                root.invoker.clone(),
                root.successor.clone(),
                root.newer_owner.clone(),
            )
        });
        let (active, queued) = cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| {
                root.present(window, cx);
                let queued = root.queue_completion("queued-superseded-successor", window, cx);
                let active = root
                    .completion
                    .clone()
                    .expect("active completion should exist");
                (active, queued)
            })
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            queued
                .complete(window, Some(queued_successor.clone()), cx)
                .expect("queued Dialog should complete");
            active
                .complete(window, Some(active_successor.clone()), cx)
                .expect("active predecessor should complete");
        });
        cx.run_until_parked();

        assert!(
            cx.update(|window, _| active_successor.is_focused(window))
                && !cx.update(|window, _| queued_successor.is_focused(window))
        );
    }

    #[gpui::test]
    fn dialog_programmatic_completion_rejects_another_operating_system_window(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_completion_focus_window(cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let completion = root
            .read_with(cx, |root, _| root.completion.clone())
            .expect("completion should be retained");
        let other_window = cx.update(|_, cx| {
            cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| DialogMenuBody))
                .expect("second Operating-System Window should open")
        });

        let result = cx.update(|_, cx| {
            cx.update_window(other_window.into(), |_, window, cx| {
                completion.complete(window, None, cx)
            })
            .expect("second Operating-System Window should remain available")
        });

        assert!(matches!(
            result,
            Err(ModalTerminalOutcomeError::Stale(error))
                if error.attempted() == completion.presentation_id() && error.current().is_none()
        ));
    }

    #[gpui::test]
    fn dialog_programmatic_completion_ignores_a_removed_successor(cx: &mut TestAppContext) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, successor) =
            root.read_with(cx, |root, _| (root.invoker.clone(), root.successor.clone()));
        cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        root.update(cx, |root, cx| {
            root.show_successor = false;
            cx.notify();
        });
        cx.run_until_parked();
        let completion = root
            .read_with(cx, |root, _| root.completion.clone())
            .expect("completion should be retained");

        cx.update(|window, cx| {
            completion
                .complete(window, Some(successor.clone()), cx)
                .expect("Dialog should complete");
        });
        cx.run_until_parked();

        assert!(!cx.update(|window, _| successor.is_focused(window)));
        assert!(!cx.update(|window, _| invoker.is_focused(window)));
    }

    #[gpui::test]
    fn dialog_programmatic_completion_does_not_steal_from_a_newer_focus_owner(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_completion_focus_window(cx);
        let (invoker, successor, newer_owner) = root.read_with(cx, |root, _| {
            (
                root.invoker.clone(),
                root.successor.clone(),
                root.newer_owner.clone(),
            )
        });
        cx.update(|window, cx| {
            invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        let completion = root
            .read_with(cx, |root, _| root.completion.clone())
            .expect("completion should be retained");

        cx.update(|window, cx| {
            completion
                .complete(window, Some(successor.clone()), cx)
                .expect("Dialog should complete");
            newer_owner.focus(window);
        });
        cx.run_until_parked();

        assert!(cx.update(|window, _| newer_owner.is_focused(window)));
        assert!(!cx.update(|window, _| successor.is_focused(window)));
    }

    struct DialogFocusBody {
        first: Entity<TextInput>,
        second: Entity<TextInput>,
        show_second: bool,
    }

    impl Render for DialogFocusBody {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(div().h(px(28.0)).child(self.first.clone()))
                .when(self.show_second, |body| {
                    body.child(div().h(px(28.0)).child(self.second.clone()))
                })
        }
    }

    struct DialogFocusFixture {
        invoker: FocusHandle,
        body: Entity<DialogFocusBody>,
        requests: Rc<RefCell<Vec<&'static str>>>,
        deny_with_first_invalid: bool,
        presentation: Option<super::super::DialogCompletion>,
    }

    impl DialogFocusFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let initial = self.body.read(cx).first.read(cx).focus_handle();
            let first_invalid = self.deny_with_first_invalid.then(|| initial.clone());
            let requests = self.requests.clone();
            self.presentation = Some(
                Dialog::new(
                    ModalId::new("dialog-focus"),
                    "Dialog focus traversal",
                    "Focus Traversal",
                    vec![
                        ModalAction::new("help", "Help", ModalActionRole::Help, "focus-help"),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "focus-cancel",
                        ),
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "focus-save",
                        )
                        .default_action(true),
                    ],
                    DialogInitialFocus::Body(initial),
                )
                .body(self.body.clone())
                .present(
                    window,
                    cx,
                    move |request, _, _| {
                        requests.borrow_mut().push(*request.action_id());
                        DialogCloseDecision::Deny {
                            first_invalid: first_invalid.clone(),
                        }
                    },
                    |_, _| {},
                )
                .expect("focus Dialog should present"),
            );
        }
    }

    impl Render for DialogFocusFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.invoker)
                    .child("Dialog underlay"),
            )
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum DialogFocusTarget {
        Underlay,
        First,
        Second,
        Help,
        Cancel,
        Save,
        Other,
    }

    fn dialog_focus_target(
        root: &Entity<DialogFocusFixture>,
        window: &mut Window,
        cx: &mut App,
    ) -> DialogFocusTarget {
        let (invoker, body) =
            root.read_with(cx, |root, _| (root.invoker.clone(), root.body.clone()));
        let (first, second) = body.read_with(cx, |body, cx| {
            (
                body.first.read(cx).focus_handle(),
                body.second.read(cx).focus_handle(),
            )
        });
        if invoker.is_focused(window) {
            DialogFocusTarget::Underlay
        } else if first.is_focused(window) {
            DialogFocusTarget::First
        } else if second.is_focused(window) {
            DialogFocusTarget::Second
        } else {
            DialogFocusTarget::Other
        }
    }

    fn press_space(cx: &mut VisualTestContext) {
        let keystroke = Keystroke::parse("space").unwrap_or_default();
        cx.simulate_event(KeyDownEvent {
            keystroke: keystroke.clone(),
            is_held: false,
        });
        cx.simulate_event(KeyUpEvent { keystroke });
        cx.run_until_parked();
    }

    fn observe_dialog_focus_target(
        root: &Entity<DialogFocusFixture>,
        cx: &mut VisualTestContext,
    ) -> DialogFocusTarget {
        let known = cx.update(|window, cx| dialog_focus_target(root, window, cx));
        if known != DialogFocusTarget::Other {
            return known;
        }
        let (requests, before) = root.read_with(cx, |root, _| {
            (root.requests.clone(), root.requests.borrow().len())
        });
        press_space(cx);
        match requests.borrow().get(before).copied() {
            Some("help") => DialogFocusTarget::Help,
            Some("cancel") => DialogFocusTarget::Cancel,
            Some("save") => DialogFocusTarget::Save,
            _ => DialogFocusTarget::Other,
        }
    }

    fn dialog_focus_window(
        cx: &mut TestAppContext,
        direction: TextDirection,
    ) -> (Entity<DialogFocusFixture>, &mut VisualTestContext) {
        install_test_catalogs(cx);
        cx.set_global(ModalDesktopPolicy::mac_os().with_text_direction(direction));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let first = cx.new(|cx| {
                TextInput::new("focus-first", "First field", "", window, cx)
                    .return_behavior(TextInputReturnBehavior::Propagate)
            });
            let second = cx.new(|cx| {
                TextInput::new("focus-second", "Second field", "", window, cx)
                    .return_behavior(TextInputReturnBehavior::Propagate)
            });
            DialogFocusFixture {
                invoker: cx.focus_handle().tab_stop(true),
                body: cx.new(|_| DialogFocusBody {
                    first,
                    second,
                    show_second: true,
                }),
                requests: root_requests,
                deny_with_first_invalid: false,
                presentation: None,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.read(cx).invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        (root, cx)
    }

    #[gpui::test]
    fn dialog_denial_refocuses_the_original_initial_field_without_mutating_values(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let first = cx.new(|cx| {
                TextInput::new("denial-first", "First field", "alpha", window, cx)
                    .return_behavior(TextInputReturnBehavior::Propagate)
            });
            let second = cx.new(|cx| {
                TextInput::new("denial-second", "Second field", "beta", window, cx)
                    .return_behavior(TextInputReturnBehavior::Propagate)
            });
            DialogFocusFixture {
                invoker: cx.focus_handle().tab_stop(true),
                body: cx.new(|_| DialogFocusBody {
                    first,
                    second,
                    show_second: true,
                }),
                requests: root_requests,
                deny_with_first_invalid: true,
                presentation: None,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.read(cx).invoker.focus(window);
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let (first, second) = root.read_with(cx, |root, cx| {
            let body = root.body.read(cx);
            (body.first.clone(), body.second.clone())
        });
        let first_focus = first.read_with(cx, |input, _| input.focus_handle());
        let values_are_unchanged = first.read_with(cx, |input, _| input.value() == "alpha")
            && second.read_with(cx, |input, _| input.value() == "beta");
        assert!(cx.update(|window, _| first_focus.is_focused(window)) && values_are_unchanged);
    }

    #[gpui::test]
    fn dialog_tab_order_includes_all_body_help_and_decision_stops_in_both_directions(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_focus_window(cx, TextDirection::LeftToRight);
        let mut actual = vec![observe_dialog_focus_target(&root, cx)];
        for key in [
            "tab",
            "tab",
            "tab",
            "tab",
            "tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
        ] {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
            actual.push(observe_dialog_focus_target(&root, cx));
        }

        assert_eq!(
            actual,
            vec![
                DialogFocusTarget::First,
                DialogFocusTarget::Second,
                DialogFocusTarget::Help,
                DialogFocusTarget::Cancel,
                DialogFocusTarget::Save,
                DialogFocusTarget::First,
                DialogFocusTarget::Save,
                DialogFocusTarget::Cancel,
                DialogFocusTarget::Help,
                DialogFocusTarget::Second,
                DialogFocusTarget::First,
            ]
        );
    }

    #[gpui::test]
    fn right_to_left_dialog_tab_order_follows_logical_policy_not_button_geometry(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_focus_window(cx, TextDirection::RightToLeft);
        let mut actual = vec![observe_dialog_focus_target(&root, cx)];
        for key in [
            "tab",
            "tab",
            "tab",
            "tab",
            "tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
            "shift-tab",
        ] {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
            actual.push(observe_dialog_focus_target(&root, cx));
        }

        assert_eq!(
            actual,
            vec![
                DialogFocusTarget::First,
                DialogFocusTarget::Second,
                DialogFocusTarget::Help,
                DialogFocusTarget::Cancel,
                DialogFocusTarget::Save,
                DialogFocusTarget::First,
                DialogFocusTarget::Save,
                DialogFocusTarget::Cancel,
                DialogFocusTarget::Help,
                DialogFocusTarget::Second,
                DialogFocusTarget::First,
            ]
        );
    }

    #[gpui::test]
    fn dialog_repairs_disabled_and_removed_body_focus_to_the_first_live_tab_stop(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_focus_window(cx, TextDirection::LeftToRight);
        let body = root.read_with(cx, |root, _| root.body.clone());
        let first = body.read_with(cx, |body, _| body.first.clone());
        first.update(cx, |input, cx| input.set_enabled(false, cx));
        cx.run_until_parked();
        let after_disable = cx.update(|window, cx| dialog_focus_target(&root, window, cx));

        body.update(cx, |body, cx| {
            body.show_second = false;
            cx.notify();
        });
        cx.run_until_parked();
        let after_remove = observe_dialog_focus_target(&root, cx);

        assert_eq!(
            (after_disable, after_remove),
            (DialogFocusTarget::Second, DialogFocusTarget::Help)
        );
    }

    struct AlertFocusFixture {
        invoker: FocusHandle,
        outcome: Rc<RefCell<Option<AlertOutcome<&'static str>>>>,
        presentation: Option<super::super::ModalPresentationHandle>,
    }

    impl AlertFocusFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            *self.outcome.borrow_mut() = None;
            let outcome = self.outcome.clone();
            self.presentation = Some(
                Alert::new(
                    ModalId::new("alert-focus"),
                    "Alert focus traversal",
                    "Focus Traversal",
                    "Every Alert control remains keyboard reachable.",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "alert-focus-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "alert-focus-cancel",
                        ),
                    ],
                )
                .help_action(ModalAction::new(
                    "help",
                    "Help",
                    ModalActionRole::Help,
                    "alert-focus-help",
                ))
                .suppression(AlertSuppression::new("Do not ask again", false))
                .present(window, cx, move |result, _| {
                    *outcome.borrow_mut() = Some(result);
                })
                .expect("focus Alert should present"),
            );
        }
    }

    impl Render for AlertFocusFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(
                div()
                    .size_full()
                    .track_focus(&self.invoker)
                    .child("Alert underlay"),
            )
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum AlertFocusTarget {
        Underlay,
        Suppression,
        Help,
        Cancel,
        Save,
    }

    fn observe_alert_focus_target(
        root: &Entity<AlertFocusFixture>,
        keys: &[&str],
        cx: &mut VisualTestContext,
    ) -> AlertFocusTarget {
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        for key in keys {
            cx.simulate_keystrokes(key);
            cx.run_until_parked();
        }
        if cx.update(|window, cx| root.read(cx).invoker.is_focused(window)) {
            return AlertFocusTarget::Underlay;
        }

        press_space(cx);
        let outcome = root.read_with(cx, |root, _| root.outcome.borrow().clone());
        let target = match outcome {
            Some(AlertOutcome::Activated {
                action_id: "help", ..
            }) => AlertFocusTarget::Help,
            Some(AlertOutcome::Activated {
                action_id: "cancel",
                ..
            }) => AlertFocusTarget::Cancel,
            Some(AlertOutcome::Activated {
                action_id: "save", ..
            }) => AlertFocusTarget::Save,
            None => AlertFocusTarget::Suppression,
            Some(other) => panic!("unexpected Alert focus outcome: {other:?}"),
        };
        if cx.update(|window, cx| super::super::window_modal_is_open(window, cx)) {
            let presentation = root
                .read_with(cx, |root, _| root.presentation.clone())
                .expect("open Alert should retain its presentation");
            cx.update(|window, cx| {
                presentation
                    .dismiss(window, cx)
                    .expect("observed Alert should dismiss");
            });
            cx.run_until_parked();
        }
        target
    }

    #[gpui::test]
    fn alert_tab_order_includes_suppression_help_and_decisions_without_underlay_escape(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFocusFixture {
            invoker: cx.focus_handle().tab_stop(true),
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let actual = vec![
            observe_alert_focus_target(&root, &[], cx),
            observe_alert_focus_target(&root, &["tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab", "tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab", "tab", "tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab", "shift-tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab", "shift-tab", "shift-tab"], cx),
            observe_alert_focus_target(
                &root,
                &["shift-tab", "shift-tab", "shift-tab", "shift-tab"],
                cx,
            ),
        ];

        assert_eq!(
            actual,
            vec![
                AlertFocusTarget::Save,
                AlertFocusTarget::Cancel,
                AlertFocusTarget::Suppression,
                AlertFocusTarget::Help,
                AlertFocusTarget::Save,
                AlertFocusTarget::Help,
                AlertFocusTarget::Suppression,
                AlertFocusTarget::Cancel,
                AlertFocusTarget::Save,
            ]
        );
    }

    #[gpui::test]
    fn right_to_left_alert_tab_order_keeps_suppression_help_and_logical_decisions_contained(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        cx.set_global(ModalDesktopPolicy::mac_os().with_text_direction(TextDirection::RightToLeft));
        let outcome = Rc::new(RefCell::new(None));
        let root_outcome = outcome.clone();
        let (root, cx) = cx.add_window_view(move |_, cx| AlertFocusFixture {
            invoker: cx.focus_handle().tab_stop(true),
            outcome: root_outcome,
            presentation: None,
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();

        let actual = vec![
            observe_alert_focus_target(&root, &[], cx),
            observe_alert_focus_target(&root, &["tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab", "tab"], cx),
            observe_alert_focus_target(&root, &["tab", "tab", "tab", "tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab", "shift-tab"], cx),
            observe_alert_focus_target(&root, &["shift-tab", "shift-tab", "shift-tab"], cx),
            observe_alert_focus_target(
                &root,
                &["shift-tab", "shift-tab", "shift-tab", "shift-tab"],
                cx,
            ),
        ];

        assert_eq!(
            actual,
            vec![
                AlertFocusTarget::Save,
                AlertFocusTarget::Cancel,
                AlertFocusTarget::Suppression,
                AlertFocusTarget::Help,
                AlertFocusTarget::Save,
                AlertFocusTarget::Help,
                AlertFocusTarget::Suppression,
                AlertFocusTarget::Cancel,
                AlertFocusTarget::Save,
            ]
        );
    }

    struct CompositionDialogBody {
        input: Entity<TextInput>,
    }

    impl Render for CompositionDialogBody {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().h(px(28.0)).child(self.input.clone())
        }
    }

    struct CompositionDialogFixture {
        input: Entity<TextInput>,
        body: Entity<CompositionDialogBody>,
        requests: Rc<RefCell<Vec<(&'static str, ModalActivationSource)>>>,
        outcome: Rc<RefCell<Option<DialogOutcome<&'static str>>>>,
        lifecycle: Rc<RefCell<Vec<ModalLifecycleEvent>>>,
        presentation: Option<super::super::DialogCompletion>,
    }

    impl CompositionDialogFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let requests = self.requests.clone();
            let outcome = self.outcome.clone();
            let lifecycle = self.lifecycle.clone();
            self.presentation = Some(
                Dialog::new(
                    ModalId::new("composition-dialog"),
                    "Dialog input composition",
                    "Edit Name",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "composition-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "composition-cancel",
                        ),
                    ],
                    DialogInitialFocus::Body(self.input.read(cx).focus_handle()),
                )
                .body(self.body.clone())
                .present_with_lifecycle(
                    window,
                    cx,
                    move |request, _, _| {
                        requests
                            .borrow_mut()
                            .push((*request.action_id(), request.source()));
                        DialogCloseDecision::Allow
                    },
                    move |result, _| *outcome.borrow_mut() = Some(result),
                    move |event, _| lifecycle.borrow_mut().push(*event),
                )
                .expect("composition Dialog should present"),
            );
        }
    }

    impl Render for CompositionDialogFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(div().size_full())
        }
    }

    #[gpui::test]
    fn focused_dialog_text_input_receives_printable_key_input(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let outcome = Rc::new(RefCell::new(None));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| TextInput::new("printable-input", "Name", "", window, cx));
            let body = cx.new(|_| CompositionDialogBody {
                input: input.clone(),
            });
            CompositionDialogFixture {
                input,
                body,
                requests,
                outcome,
                lifecycle,
                presentation: None,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        let input = root.read_with(cx, |root, _| root.input.clone());

        cx.simulate_input("a");
        cx.run_until_parked();

        assert_eq!(
            cx.update(|window, cx| {
                (
                    input.read(cx).focus_handle().is_focused(window),
                    input.read(cx).value().to_owned(),
                )
            }),
            (true, "a".to_owned()),
        );
    }

    #[gpui::test]
    fn return_commits_dialog_input_composition_before_a_later_default_submission(
        cx: &mut TestAppContext,
    ) {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let outcome = Rc::new(RefCell::new(None));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let root_outcome = outcome.clone();
        let root_lifecycle = lifecycle.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| {
                TextInput::new("composition-input", "Name", "", window, cx)
                    .return_behavior(TextInputReturnBehavior::Propagate)
                    .escape_behavior(TextInputEscapeBehavior::Propagate)
            });
            let body = cx.new(|_| CompositionDialogBody {
                input: input.clone(),
            });
            CompositionDialogFixture {
                input,
                body,
                requests: root_requests,
                outcome: root_outcome,
                lifecycle: root_lifecycle,
                presentation: None,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        let input = root.read_with(cx, |root, _| root.input.clone());
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
            });
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            input.read_with(cx, |input, _| input.composition().is_none())
                && requests.borrow().is_empty()
                && outcome.borrow().is_none()
                && cx.update(|window, cx| super::super::window_modal_is_open(window, cx))
        );

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            (requests.borrow().clone(), outcome.borrow().clone()),
            (
                vec![("save", ModalActivationSource::Return)],
                Some(DialogOutcome::Completed {
                    action_id: "save",
                    source: ModalActivationSource::Return,
                }),
            )
        );
    }

    #[gpui::test]
    fn escape_cancels_dialog_input_composition_before_later_safe_cancel(cx: &mut TestAppContext) {
        install_test_catalogs(cx);
        let requests = Rc::new(RefCell::new(Vec::new()));
        let outcome = Rc::new(RefCell::new(None));
        let lifecycle = Rc::new(RefCell::new(Vec::new()));
        let root_requests = requests.clone();
        let root_outcome = outcome.clone();
        let root_lifecycle = lifecycle.clone();
        let (root, cx) = cx.add_window_view(move |window, cx| {
            let input = cx.new(|cx| {
                TextInput::new("escape-composition-input", "Name", "", window, cx)
                    .escape_behavior(TextInputEscapeBehavior::Propagate)
            });
            let body = cx.new(|_| CompositionDialogBody {
                input: input.clone(),
            });
            CompositionDialogFixture {
                input,
                body,
                requests: root_requests,
                outcome: root_outcome,
                lifecycle: root_lifecycle,
                presentation: None,
            }
        });
        cx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |root, cx| root.present(window, cx));
        });
        cx.run_until_parked();
        let (input, presentation) = root.read_with(cx, |root, _| {
            (
                root.input.clone(),
                root.presentation
                    .as_ref()
                    .expect("Dialog completion should be retained")
                    .presentation_id(),
            )
        });
        cx.update(|window, cx| {
            input.update(cx, |input, cx| {
                input.replace_and_mark_text_in_range(None, "日本", None, window, cx);
            });
        });

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(
            input.read_with(cx, |input, _| input.composition().is_none())
                && requests.borrow().is_empty()
                && outcome.borrow().is_none()
                && lifecycle.borrow().as_slice() == [ModalLifecycleEvent::Opened(presentation)]
                && cx.update(|window, cx| super::super::window_modal_is_open(window, cx)),
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert_eq!(
            (
                requests.borrow().clone(),
                outcome.borrow().clone(),
                lifecycle.borrow().clone(),
                cx.update(|window, cx| super::super::window_modal_is_open(window, cx)),
            ),
            (
                vec![("cancel", ModalActivationSource::Escape)],
                Some(DialogOutcome::Completed {
                    action_id: "cancel",
                    source: ModalActivationSource::Escape,
                }),
                vec![
                    ModalLifecycleEvent::Opened(presentation),
                    ModalLifecycleEvent::ActionRequested(presentation),
                    ModalLifecycleEvent::Closing(presentation),
                    ModalLifecycleEvent::Closed(presentation, ModalCloseReason::Cancelled),
                ],
                false,
            ),
        );
    }

    struct DialogMenuBody;

    impl Render for DialogMenuBody {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            Menu::new(
                "dialog-owned-menu",
                "Dialog options",
                vec![MenuEntry::action("Choose option", ())],
            )
            .size(MenuSize::Small)
            .debug_selector("dialog-owned-menu-trigger")
            .on_activate(|_, _, _| {})
        }
    }

    struct DialogMenuFixture {
        body: Entity<DialogMenuBody>,
        presentation: Option<super::super::DialogCompletion>,
        allow_actions: bool,
        outcome: Rc<RefCell<Option<DialogOutcome<&'static str>>>>,
    }

    impl DialogMenuFixture {
        fn present(&mut self, window: &Window, cx: &mut Context<Self>) {
            let allow_actions = self.allow_actions;
            let outcome = self.outcome.clone();
            self.presentation = Some(
                Dialog::new(
                    ModalId::new("dialog-owned-menu-fixture"),
                    "Dialog with options",
                    "Edit Options",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "dialog-save",
                        )
                        .default_action(true),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "dialog-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("save"),
                )
                .body(self.body.clone())
                .present(
                    window,
                    cx,
                    move |_, _, _| {
                        if allow_actions {
                            DialogCloseDecision::Allow
                        } else {
                            DialogCloseDecision::Deny {
                                first_invalid: None,
                            }
                        }
                    },
                    move |result, _| *outcome.borrow_mut() = Some(result),
                )
                .expect("dialog should present"),
            );
        }
    }

    impl Render for DialogMenuFixture {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            ModalLayer::new(TooltipLayer::new(
                div().size_full().child(
                    Menu::new(
                        "underlay-menu",
                        "Underlay options",
                        vec![MenuEntry::action("Underlay action", ())],
                    )
                    .size(MenuSize::Small)
                    .debug_selector("underlay-menu-trigger")
                    .on_activate(|_, _, _| {}),
                ),
            ))
        }
    }

    fn dialog_menu_window(
        cx: &mut TestAppContext,
    ) -> (Entity<DialogMenuFixture>, &mut VisualTestContext) {
        install_test_catalogs(cx);
        let (root, cx) = cx.add_window_view(|_, cx| DialogMenuFixture {
            body: cx.new(|_| DialogMenuBody),
            presentation: None,
            allow_actions: false,
            outcome: Rc::new(RefCell::new(None)),
        });
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        (root, cx)
    }

    #[gpui::test]
    fn unrelated_menu_should_be_dismissed_before_the_first_modal_opens(cx: &mut TestAppContext) {
        let (root, cx) = dialog_menu_window(cx);
        let trigger = cx
            .debug_bounds("underlay-menu-trigger")
            .expect("underlay menu trigger should render");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));

        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
        assert!(cx.debug_bounds("modal-surface-1").is_some());
    }

    #[gpui::test]
    fn dialog_owned_menu_escape_should_restore_inside_the_dialog_without_closing_it(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_menu_window(cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let trigger = cx
            .debug_bounds("dialog-owned-menu-trigger")
            .expect("dialog menu trigger should render");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| {
            crate::menu::window_menu_is_owned_by_current_modal(window, cx)
        }));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));

        cx.simulate_keystrokes("space");
        cx.run_until_parked();
        assert!(cx.update(|window, cx| {
            crate::menu::window_menu_is_owned_by_current_modal(window, cx)
        }));
    }

    #[gpui::test]
    fn programmatic_dialog_completion_closes_owned_menu_before_parent(cx: &mut TestAppContext) {
        let (root, cx) = dialog_menu_window(cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let trigger = cx
            .debug_bounds("dialog-owned-menu-trigger")
            .expect("dialog menu trigger should render");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        let completion = root
            .read_with(cx, |root, _| root.presentation.clone())
            .expect("Dialog completion should be retained");

        cx.update(|window, cx| {
            completion
                .complete(window, None, cx)
                .expect("Dialog should complete programmatically");
        });
        cx.run_until_parked();
        let duplicate = cx.update(|window, cx| completion.complete(window, None, cx));

        assert_eq!(duplicate, Err(ModalTerminalOutcomeError::AlreadyDelivered));
        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
        assert!(!cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert_eq!(
            root.read_with(cx, |root, _| root.outcome.borrow().clone()),
            Some(DialogOutcome::ProgrammaticallyCompleted)
        );
    }

    #[gpui::test]
    fn public_dialog_replacement_retires_owned_menu_before_successor_open(cx: &mut TestAppContext) {
        let (root, cx) = dialog_menu_window(cx);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let trigger = cx
            .debug_bounds("dialog-owned-menu-trigger")
            .expect("dialog menu trigger should render");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();
        assert!(cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));

        let replacement = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                Dialog::new(
                    ModalId::new("owned-menu-replacement-dialog"),
                    "Owned Menu replacement Dialog",
                    "Replacement Dialog",
                    vec![
                        ModalAction::new(
                            "finish",
                            "Finish",
                            ModalActionRole::Affirmative,
                            "owned-menu-replacement-finish",
                        ),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "owned-menu-replacement-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("finish"),
                )
                .replace_active(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Pending,
                    |_, _| {},
                    |_, _| {},
                )
                .expect("Dialog should replace owned-Menu predecessor")
            })
        });
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
        assert!(cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert_eq!(
            root.read_with(cx, |root, _| root.outcome.borrow().clone()),
            Some(DialogOutcome::Dismissed(ModalCloseReason::Replaced))
        );
        cx.update(|window, cx| {
            replacement
                .complete(window, None, cx)
                .expect("replacement Dialog should complete");
        });
    }

    #[gpui::test]
    fn queued_dialog_programmatic_completion_delivers_without_opening(cx: &mut TestAppContext) {
        let (root, _, _, cx) = alert_window(cx);
        let outcome = Rc::new(RefCell::new(None));
        let result_outcome = outcome.clone();
        let completion = cx.update(|window, cx| {
            root.update(cx, |_, cx| {
                Dialog::new(
                    ModalId::new("queued-programmatic-dialog"),
                    "Queued programmatic completion",
                    "Queued Dialog",
                    vec![
                        ModalAction::new(
                            "save",
                            "Save",
                            ModalActionRole::Affirmative,
                            "queued-dialog-save",
                        ),
                        ModalAction::new(
                            "cancel",
                            "Cancel",
                            ModalActionRole::Cancel,
                            "queued-dialog-cancel",
                        ),
                    ],
                    DialogInitialFocus::Action("save"),
                )
                .present(
                    window,
                    cx,
                    |_, _, _| DialogCloseDecision::Pending,
                    move |result, _| *result_outcome.borrow_mut() = Some(result),
                )
                .expect("Dialog should queue")
            })
        });

        cx.update(|window, cx| {
            completion
                .complete(window, None, cx)
                .expect("queued Dialog should complete");
        });
        cx.run_until_parked();

        assert_eq!(
            outcome.borrow().clone(),
            Some(DialogOutcome::ProgrammaticallyCompleted)
        );
        assert!(cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert!(cx.debug_bounds("modal-surface-2").is_none());
    }

    #[gpui::test]
    fn command_period_closes_owned_menu_before_allowed_parent_cancellation(
        cx: &mut TestAppContext,
    ) {
        let (root, cx) = dialog_menu_window(cx);
        root.update(cx, |root, _| root.allow_actions = true);
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();
        let trigger = cx
            .debug_bounds("dialog-owned-menu-trigger")
            .expect("dialog menu trigger should render");
        cx.simulate_click(trigger.center(), Modifiers::default());
        cx.run_until_parked();

        cx.simulate_keystrokes("cmd-.");
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
        assert!(!cx.update(|window, cx| super::super::window_modal_is_open(window, cx)));
        assert_eq!(
            root.read_with(cx, |root, _| root.outcome.borrow().clone()),
            Some(DialogOutcome::Completed {
                action_id: "cancel",
                source: ModalActivationSource::CommandPeriod,
            })
        );
    }

    #[gpui::test]
    fn underlay_menu_attempt_should_be_blocked_while_a_modal_is_open(cx: &mut TestAppContext) {
        let (root, cx) = dialog_menu_window(cx);
        let underlay = cx
            .debug_bounds("underlay-menu-trigger")
            .expect("underlay menu trigger should render");
        cx.update(|window, cx| root.update(cx, |root, cx| root.present(window, cx)));
        cx.run_until_parked();

        cx.simulate_click(underlay.center(), Modifiers::default());
        cx.run_until_parked();

        assert!(!cx.update(|window, cx| crate::menu::window_menu_is_open(window, cx)));
    }

    #[test]
    fn surface_contains_includes_edges_and_rejects_scrim() {
        let geometry = ModalSurfaceGeometry {
            origin_x: px(10.0),
            origin_y: px(20.0),
            size: size(px(100.0), px(80.0)),
        };

        assert!(surface_contains(geometry, gpui::point(px(10.0), px(20.0))));
        assert!(!surface_contains(geometry, gpui::point(px(9.0), px(20.0))));
    }

    #[test]
    fn default_presentation_is_policy_resolved_without_mutating_action_semantics() {
        let action = ModalRenderAction {
            label: "Continue".into(),
            role: ModalActionRole::Affirmative,
            intent: ModalActionIntent::Ordinary,
            emphasis: ModalActionEmphasis::Standard,
            enabled: true,
            is_default: true,
            debug_identity: "policy-default".into(),
        };

        assert_eq!(
            (
                ModalDesktopPolicy::mac_os().default_action_presentation(&action),
                ModalDesktopPolicy::win_ui_for_tests().default_action_presentation(&action),
                action.role,
                action.intent,
                action.emphasis,
            ),
            (
                DefaultActionPresentation::Ring,
                DefaultActionPresentation::None,
                ModalActionRole::Affirmative,
                ModalActionIntent::Ordinary,
                ModalActionEmphasis::Standard,
            )
        );
    }

    #[test]
    fn modal_default_and_cancel_resolution_uses_semantics_not_position() {
        let actions = vec![
            ModalRenderAction {
                label: "Cancel".into(),
                role: ModalActionRole::Cancel,
                intent: ModalActionIntent::Ordinary,
                emphasis: ModalActionEmphasis::Standard,
                enabled: true,
                is_default: false,
                debug_identity: "cancel".into(),
            },
            ModalRenderAction {
                label: "Save".into(),
                role: ModalActionRole::Affirmative,
                intent: ModalActionIntent::Ordinary,
                emphasis: ModalActionEmphasis::Standard,
                enabled: true,
                is_default: true,
                debug_identity: "save".into(),
            },
        ];

        assert_eq!(
            (
                enabled_action(Some(1), &actions),
                enabled_action(Some(0), &actions)
            ),
            (Some(1), Some(0))
        );
    }
}
