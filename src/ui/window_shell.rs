//! A client frame around application content, driven by the window's decoration facts.

use gpui::{
    AnyElement, App, Bounds, ClientFrame, Corners, CursorStyle, Decorations, Edges, MouseButton,
    MouseDownEvent, Pixels, ResizeEdge, Size, Tiling, Window, canvas, div, point, prelude::*, px,
    rgba, size,
};

#[cfg(test)]
use crate::platform::window_chrome::CLIENT_FRAME_INSET;
use crate::platform::window_chrome::{frame_inset, frame_shadows};

const RESIZE_BAND: f32 = 10.0;
const CORNER_TARGET: f32 = 24.0;

#[derive(Default)]
struct PublishedInputRegion {
    region: Option<(Option<Bounds<Pixels>>, f32)>,
}

impl PublishedInputRegion {
    fn publish(
        &mut self,
        region: Option<Bounds<Pixels>>,
        scale_factor: f32,
        publish: impl FnOnce(Option<&[Bounds<Pixels>]>),
    ) {
        if self.region == Some((region, scale_factor)) {
            return;
        }
        match region {
            Some(bounds) => publish(Some(&[bounds])),
            None => publish(None),
        }
        self.region = Some((region, scale_factor));
    }
}

fn publish_input_region(region: Option<Bounds<Pixels>>, window: &mut Window, cx: &mut App) {
    let state = window.use_keyed_state("client-window-input-region", cx, |_, _| {
        PublishedInputRegion::default()
    });
    state.update(cx, |state, _| {
        state.publish(region, window.scale_factor(), |region| {
            window.set_input_region(region)
        })
    });
}

fn padding(tiling: Tiling, frame_inset: f32) -> Edges<Pixels> {
    Edges {
        top: px(if tiling.top { 0.0 } else { frame_inset }),
        right: px(if tiling.right { 0.0 } else { frame_inset }),
        bottom: px(if tiling.bottom { 0.0 } else { frame_inset }),
        left: px(if tiling.left { 0.0 } else { frame_inset }),
    }
}

fn resize_regions(
    viewport: Size<Pixels>,
    tiling: Tiling,
    frame_inset: f32,
) -> Vec<(Bounds<Pixels>, ResizeEdge)> {
    let inset = padding(tiling, frame_inset);
    let left = inset.left;
    let top = inset.top;
    let right = viewport.width - inset.right;
    let bottom = viewport.height - inset.bottom;
    let band = px(RESIZE_BAND);
    let opaque = frame_inset == 0.0;
    let left = if opaque { band } else { left };
    let top = if opaque { band } else { top };
    let right = if opaque { right - band } else { right };
    let bottom = if opaque { bottom - band } else { bottom };
    let corner = px(CORNER_TARGET);
    let mut regions = Vec::with_capacity(8);
    let mut add = |x1, y1, x2, y2, edge| {
        if x2 > x1 && y2 > y1 {
            regions.push((Bounds::new(point(x1, y1), size(x2 - x1, y2 - y1)), edge));
        }
    };
    if !tiling.top {
        add(left, top - band, right, top, ResizeEdge::Top);
    }
    if !tiling.bottom {
        add(left, bottom, right, bottom + band, ResizeEdge::Bottom);
    }
    if !tiling.left {
        add(left - band, top, left, bottom, ResizeEdge::Left);
    }
    if !tiling.right {
        add(right, top, right + band, bottom, ResizeEdge::Right);
    }
    // Corner hitboxes are inserted after the edges so they take precedence where they overlap.
    if !tiling.top && !tiling.left {
        add(
            left - band,
            top - band,
            left + corner,
            top,
            ResizeEdge::TopLeft,
        );
        add(left - band, top, left, top + corner, ResizeEdge::TopLeft);
    }
    if !tiling.top && !tiling.right {
        add(
            right - corner,
            top - band,
            right + band,
            top,
            ResizeEdge::TopRight,
        );
        add(right, top, right + band, top + corner, ResizeEdge::TopRight);
    }
    if !tiling.bottom && !tiling.left {
        add(
            left - band,
            bottom,
            left + corner,
            bottom + band,
            ResizeEdge::BottomLeft,
        );
        add(
            left - band,
            bottom - corner,
            left,
            bottom,
            ResizeEdge::BottomLeft,
        );
    }
    if !tiling.bottom && !tiling.right {
        add(
            right - corner,
            bottom,
            right + band,
            bottom + band,
            ResizeEdge::BottomRight,
        );
        add(
            right,
            bottom - corner,
            right + band,
            bottom,
            ResizeEdge::BottomRight,
        );
    }
    regions
}

fn cursor(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn frame_corners(tiling: Tiling, transparent: bool, radius: f32) -> Corners<Pixels> {
    let corner = |first, second| {
        px(if first || second || !transparent {
            0.0
        } else {
            radius
        })
    };
    Corners {
        top_left: corner(tiling.top, tiling.left),
        top_right: corner(tiling.top, tiling.right),
        bottom_left: corner(tiling.bottom, tiling.left),
        bottom_right: corner(tiling.bottom, tiling.right),
    }
}

pub(super) fn render(content: impl IntoElement, window: &mut Window, cx: &mut App) -> AnyElement {
    window.use_keyed_state("client-window-frame-observer", cx, |window, _| {
        window.observe_window_appearance(|window, _| window.refresh())
    });
    let Decorations::Client { tiling } = window.window_decorations() else {
        window.set_client_inset(px(0.0));
        window.set_client_frame(None);
        publish_input_region(None, window, cx);
        return content.into_any_element();
    };
    let tiling = if window.is_fullscreen() || window.is_maximized() {
        Tiling::tiled()
    } else {
        tiling
    };
    let style = cx
        .try_global::<spaceterm_ui::DesktopWindowControls>()
        .map_or(spaceterm_ui::DesktopWindowStyle::default(), |facts| {
            facts.style
        });
    let transparent_frame = window.supports_transparent_client_frame();
    let frame_inset = if transparent_frame {
        frame_inset(style)
    } else {
        0.0
    };
    let inset = padding(tiling, frame_inset);
    // The native inset also depends on maximized and tiled edges. Republish the desired inset
    // after those facts change even when the nominal shadow width stays the same.
    window.set_client_inset(px(frame_inset));
    let active = window.is_window_active();
    window.set_client_frame(transparent_frame.then(|| {
        ClientFrame {
            inset: px(frame_inset),
            corner_radius: px(style.corner_radius()),
            tiling,
            shadows: if tiling == Tiling::tiled() {
                Vec::new()
            } else {
                frame_shadows(style, active)
            }
            .into(),
        }
    }));
    let viewport = window.viewport_size();
    let band = if window.is_resizable() {
        px(RESIZE_BAND)
    } else {
        px(0.0)
    };
    let input_left = (inset.left - band).max(px(0.0));
    let input_top = (inset.top - band).max(px(0.0));
    let input_right = (inset.right - band).max(px(0.0));
    let input_bottom = (inset.bottom - band).max(px(0.0));
    publish_input_region(
        Some(Bounds::new(
            point(input_left, input_top),
            size(
                viewport.width - input_left - input_right,
                viewport.height - input_top - input_bottom,
            ),
        )),
        window,
        cx,
    );
    let radii = frame_corners(tiling, transparent_frame, style.corner_radius());
    let dark = spaceterm_ui::window_controls_dark(super::appearance::gpui_color(
        super::appearance::chrome(cx).colors.title_bar_background,
    ));
    let edge = rgba(match style {
        spaceterm_ui::DesktopWindowStyle::Adwaita => {
            if dark {
                0xffffff12
            } else {
                0xffffff4d
            }
        }
        spaceterm_ui::DesktopWindowStyle::Breeze => {
            if dark {
                0xffffff33
            } else {
                0x00000033
            }
        }
    });
    let resizable = window.is_resizable() && !window.is_fullscreen();
    div()
        .size_full()
        .relative()
        .pt(inset.top)
        .pr(inset.right)
        .pb(inset.bottom)
        .pl(inset.left)
        .child(
            div().relative().size_full().child(content).child(
                div()
                    .absolute()
                    .inset_0()
                    .border_t(px(if tiling.top { 0.0 } else { 1.0 }))
                    .border_r(px(if tiling.right { 0.0 } else { 1.0 }))
                    .border_b(px(if tiling.bottom { 0.0 } else { 1.0 }))
                    .border_l(px(if tiling.left { 0.0 } else { 1.0 }))
                    .border_color(edge)
                    .rounded_tl(radii.top_left)
                    .rounded_tr(radii.top_right)
                    .rounded_bl(radii.bottom_left)
                    .rounded_br(radii.bottom_right),
            ),
        )
        .when(resizable, |shell| {
            shell.child(
                canvas(
                    move |bounds, window, _| {
                        resize_regions(bounds.size, tiling, frame_inset)
                            .into_iter()
                            .map(|(mut region, edge)| {
                                region.origin += bounds.origin;
                                (
                                    spaceterm_ui::ModalLayer::window_chrome_hitbox(region, window),
                                    edge,
                                )
                            })
                            .collect::<Vec<_>>()
                    },
                    |_, hitboxes, window, _| {
                        for (hitbox, edge) in &hitboxes {
                            window.set_cursor_style(cursor(*edge), hitbox);
                        }
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                            if !phase.capture() || event.button != MouseButton::Left {
                                return;
                            }
                            if let Some((_, edge)) = hitboxes
                                .iter()
                                .rev()
                                .find(|(hitbox, _)| hitbox.is_hovered(window))
                            {
                                window.start_window_resize(*edge);
                                window.prevent_default();
                                cx.stop_propagation();
                            }
                        });
                    },
                )
                .absolute()
                .inset_0(),
            )
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    #[test]
    fn input_region_publishes_only_geometry_and_decoration_changes() {
        let mut published = PublishedInputRegion::default();
        let first = Bounds::new(point(px(10.), px(10.)), size(px(800.), px(600.)));
        let resized = Bounds::new(first.origin, size(px(900.), px(600.)));
        let tiled = Bounds::new(point(px(0.), px(0.)), resized.size);
        let mut calls = Vec::new();
        for region in [
            None,
            None,
            Some(first),
            Some(first),
            Some(resized),
            Some(tiled),
            Some(tiled),
            None,
            None,
            Some(first),
        ] {
            published.publish(region, 1.0, |region| calls.push(region.map(<[_]>::to_vec)));
        }
        for scale_factor in [2.0, 2.0] {
            published.publish(Some(first), scale_factor, |region| {
                calls.push(region.map(<[_]>::to_vec))
            });
        }
        assert_eq!(
            calls,
            [
                None,
                Some(vec![first]),
                Some(vec![resized]),
                Some(vec![tiled]),
                None,
                Some(vec![first]),
                Some(vec![first]),
            ]
        );
    }

    #[test]
    fn native_frame_geometry_removes_radii_padding_and_resize_on_tiled_edges() {
        for style in [
            spaceterm_ui::DesktopWindowStyle::Adwaita,
            spaceterm_ui::DesktopWindowStyle::Breeze,
        ] {
            for mask in 0u8..16 {
                let tiling = Tiling {
                    top: mask & 1 != 0,
                    right: mask & 2 != 0,
                    bottom: mask & 4 != 0,
                    left: mask & 8 != 0,
                };
                let corners = frame_corners(tiling, true, style.corner_radius());
                assert_eq!(
                    corners.top_left,
                    px(if tiling.top || tiling.left {
                        0.0
                    } else {
                        style.corner_radius()
                    })
                );
                assert_eq!(
                    corners.top_right,
                    px(if tiling.top || tiling.right {
                        0.0
                    } else {
                        style.corner_radius()
                    })
                );
                assert_eq!(
                    corners.bottom_left,
                    px(if tiling.bottom || tiling.left {
                        0.0
                    } else {
                        style.corner_radius()
                    })
                );
                assert_eq!(
                    corners.bottom_right,
                    px(if tiling.bottom || tiling.right {
                        0.0
                    } else {
                        style.corner_radius()
                    })
                );
                let inset = padding(tiling, frame_inset(style));
                assert_eq!(inset.top == px(0.0), tiling.top);
                assert_eq!(inset.left == px(0.0), tiling.left);
                assert_eq!(inset.right == px(0.0), tiling.right);
                assert_eq!(inset.bottom == px(0.0), tiling.bottom);
            }
            assert_eq!(
                frame_corners(Tiling::tiled(), true, style.corner_radius()),
                Corners::default()
            );
            assert_eq!(
                frame_corners(Tiling::default(), false, style.corner_radius()),
                Corners::default()
            );
        }
    }

    struct ChromeHarness(Rc<Cell<usize>>);

    impl Render for ChromeHarness {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let closed = self.0.clone();
            super::render(
                div()
                    .debug_selector(|| "chrome-content".into())
                    .size_full()
                    .flex()
                    .justify_end()
                    .items_start()
                    .child(spaceterm_ui::ClientWindowControls::new(Rc::new(
                        move |_, _| closed.set(closed.get() + 1),
                    ))),
                window,
                cx,
            )
        }
    }

    #[gpui::test]
    fn client_controls_follow_desktop_changes_and_delegate_window_operations(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| super::super::init(cx).unwrap());
        let closed = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view(|_, _| ChromeHarness(closed.clone()));
        cx.simulate_decorations(Decorations::Client {
            tiling: Tiling::default(),
        });
        cx.run_until_parked();
        let minimize = cx.debug_bounds("window-minimize").unwrap().center();
        cx.simulate_click(minimize, gpui::Modifiers::none());
        let maximize = cx.debug_bounds("window-maximize").unwrap().center();
        cx.simulate_click(maximize, gpui::Modifiers::none());
        assert_eq!(
            cx.window_requests(),
            [
                gpui::TestWindowRequest::Minimize,
                gpui::TestWindowRequest::Zoom
            ]
        );
        cx.simulate_button_layout(Some(gpui::WindowButtonLayout {
            left: [None; 3],
            right: [Some(gpui::WindowButton::Close), None, None],
        }));
        cx.run_until_parked();
        assert!(cx.debug_bounds("window-minimize").is_none());
        assert!(cx.debug_bounds("window-maximize").is_none());
        cx.simulate_button_layout(None);
        cx.run_until_parked();
        assert!(cx.debug_bounds("window-minimize").is_some());
        cx.simulate_window_controls(gpui::WindowControls {
            minimize: false,
            maximize: false,
            ..Default::default()
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("window-minimize").is_none());
        assert!(cx.debug_bounds("window-maximize").is_none());
        let close = cx.debug_bounds("window-close").unwrap().center();
        cx.simulate_click(close, gpui::Modifiers::none());
        assert_eq!(closed.get(), 1);
        cx.simulate_decorations(Decorations::Server);
        cx.run_until_parked();
        assert!(cx.debug_bounds("window-close").is_none());
    }

    #[gpui::test]
    fn client_controls_publish_accessible_names_and_owner_actions(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| super::super::init(cx).unwrap());
        let closed = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view(|_, _| ChromeHarness(closed.clone()));
        cx.simulate_decorations(Decorations::Client {
            tiling: Tiling::default(),
        });
        cx.activate_accessibility();
        let tree: serde_json::Value = cx.update(|window, _| {
            serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap()
        });
        for label in ["Minimize", "Maximize", "Close"] {
            let (_, node) = tree["nodes"]
                .as_object()
                .unwrap()
                .iter()
                .find(|(_, node)| node["aria"]["label"] == label)
                .unwrap_or_else(|| {
                    panic!("client control {label} must be exposed to screen readers")
                });
            assert_eq!(node["aria"]["role"], "Button");
            cx.simulate_accessibility_action(gpui::accesskit::ActionRequest {
                action: gpui::AccessibleAction::Click,
                target_tree: gpui::accesskit::TreeId::ROOT,
                target_node: gpui::accesskit::NodeId(
                    node["accesskit_id"].as_str().unwrap().parse().unwrap(),
                ),
                data: None,
            });
        }
        assert_eq!(closed.get(), 1);
        assert_eq!(
            cx.window_requests(),
            [
                gpui::TestWindowRequest::Minimize,
                gpui::TestWindowRequest::Zoom
            ]
        );
    }

    #[gpui::test]
    fn opaque_client_frame_keeps_controls_and_resizes_without_a_shadow_gutter(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| super::super::init(cx).unwrap());
        let closed = Rc::new(Cell::new(0));
        let (_, cx) = cx.add_window_view(|_, _| ChromeHarness(closed.clone()));
        cx.simulate_decorations(Decorations::Client {
            tiling: Tiling::default(),
        });
        let handle = cx.window_handle();
        cx.simulate_transparent_client_frame_support(handle, false);
        cx.run_until_parked();
        let bounds = cx.debug_bounds("chrome-content").unwrap();
        cx.update(|window, _| {
            assert_eq!(bounds.origin, point(px(0.), px(0.)));
            assert_eq!(bounds.size, window.viewport_size());
            assert_eq!(window.client_inset(), Some(px(0.)));
        });
        let close = cx.debug_bounds("window-close").unwrap().center();
        cx.simulate_click(close, gpui::Modifiers::none());
        assert_eq!(closed.get(), 1);
        cx.simulate_click(point(px(2.), px(2.)), gpui::Modifiers::none());
        assert_eq!(
            cx.window_requests(),
            [gpui::TestWindowRequest::StartWindowResize(
                ResizeEdge::TopLeft
            )]
        );
    }

    #[gpui::test]
    fn client_resize_gutter_dispatches_only_available_edges(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| super::super::init(cx).unwrap());
        let (_, cx) = cx.add_window_view(|_, _| ChromeHarness(Rc::new(Cell::new(0))));
        cx.simulate_decorations(Decorations::Client {
            tiling: Tiling::default(),
        });
        cx.run_until_parked();
        cx.simulate_mouse_down(
            point(
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
            ),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
            ),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        assert_eq!(
            cx.window_requests(),
            [gpui::TestWindowRequest::StartWindowResize(
                ResizeEdge::TopLeft
            )]
        );
        cx.simulate_decorations(Decorations::Client {
            tiling: Tiling {
                top: true,
                left: true,
                ..Default::default()
            },
        });
        cx.run_until_parked();
        cx.simulate_click(
            point(
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
            ),
            gpui::Modifiers::none(),
        );
        assert_eq!(cx.window_requests().len(), 1);
    }

    struct ModalFrameHarness(Rc<Cell<usize>>);

    impl Render for ModalFrameHarness {
        fn render(
            &mut self,
            window: &mut Window,
            cx: &mut gpui::Context<Self>,
        ) -> impl IntoElement {
            let presses = self.0.clone();
            spaceterm_ui::ModalLayer::new(super::render(
                div().size_full().flex().items_end().child(
                    spaceterm_ui::Button::new("frame-content", "Application content")
                        .debug_selector("frame-content")
                        .on_activate(move |_, _, _| presses.set(presses.get() + 1)),
                ),
                window,
                cx,
            ))
        }
    }

    #[gpui::test]
    fn modal_client_frame_preserves_resize_bands_and_blocks_content(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| super::super::init(cx).unwrap());
        for transparent in [true, false] {
            let presses = Rc::new(Cell::new(0));
            let (root, cx) = cx.add_window_view(|_, _| ModalFrameHarness(presses.clone()));
            cx.simulate_decorations(Decorations::Client {
                tiling: Tiling::default(),
            });
            cx.simulate_transparent_client_frame_support(cx.window_handle(), transparent);
            cx.update(|window, cx| {
                window.activate_window();
                root.update(cx, |_, cx| {
                    spaceterm_ui::Dialog::new(
                        spaceterm_ui::ModalId::new("frame-modal"),
                        "Frame modal",
                        "Notice",
                        vec![spaceterm_ui::ModalAction::new(
                            "cancel",
                            "Cancel",
                            spaceterm_ui::ModalActionRole::Cancel,
                            "frame-cancel",
                        )],
                        spaceterm_ui::DialogInitialFocus::Action("cancel"),
                    )
                    .present(
                        window,
                        cx,
                        |_, _, _| spaceterm_ui::DialogCloseDecision::Deny {
                            first_invalid: None,
                        },
                        |_, _| {},
                    )
                    .unwrap();
                });
            });
            cx.run_until_parked();
            let focused = cx.update(|window, cx| window.focused(cx).unwrap());
            let viewport = cx.update(|window, _| window.viewport_size());
            let inset = if transparent {
                CLIENT_FRAME_INSET
            } else {
                RESIZE_BAND
            };
            let near = px(inset - RESIZE_BAND / 2.);
            let far_x = viewport.width - near;
            let far_y = viewport.height - near;
            for (position, edge, expected_cursor) in [
                (
                    point(viewport.width / 2., near),
                    ResizeEdge::Top,
                    CursorStyle::ResizeUpDown,
                ),
                (
                    point(viewport.width / 2., far_y),
                    ResizeEdge::Bottom,
                    CursorStyle::ResizeUpDown,
                ),
                (
                    point(near, viewport.height / 2.),
                    ResizeEdge::Left,
                    CursorStyle::ResizeLeftRight,
                ),
                (
                    point(far_x, viewport.height / 2.),
                    ResizeEdge::Right,
                    CursorStyle::ResizeLeftRight,
                ),
                (
                    point(near, near),
                    ResizeEdge::TopLeft,
                    CursorStyle::ResizeUpLeftDownRight,
                ),
                (
                    point(far_x, near),
                    ResizeEdge::TopRight,
                    CursorStyle::ResizeUpRightDownLeft,
                ),
                (
                    point(near, far_y),
                    ResizeEdge::BottomLeft,
                    CursorStyle::ResizeUpRightDownLeft,
                ),
                (
                    point(far_x, far_y),
                    ResizeEdge::BottomRight,
                    CursorStyle::ResizeUpLeftDownRight,
                ),
            ] {
                cx.simulate_mouse_move(position, None, gpui::Modifiers::none());
                cx.simulate_click(position, gpui::Modifiers::none());
                assert_eq!(
                    cx.window_requests().last(),
                    Some(&gpui::TestWindowRequest::StartWindowResize(edge)),
                    "transparent={transparent}, {edge:?}"
                );
                assert_eq!(cursor(edge), expected_cursor);
                assert!(cx.update(|window, _| focused.is_focused(window)));
            }
            let content = cx.debug_bounds("frame-content").unwrap().center();
            cx.simulate_click(content, gpui::Modifiers::none());
            assert_eq!(presses.get(), 0);
            assert_eq!(cx.window_requests().len(), 8);
            assert!(cx.update(|window, _| focused.is_focused(window)));
            assert!(cx.update(|window, cx| spaceterm_ui::window_modal_is_open(window, cx)));
        }
    }

    #[test]
    fn tiled_edges_have_neither_insets_nor_resize_targets() {
        let viewport = size(
            px(900.0 + 2.0 * CLIENT_FRAME_INSET),
            px(580.0 + 2.0 * CLIENT_FRAME_INSET),
        );
        let tiling = Tiling {
            top: true,
            left: true,
            ..Default::default()
        };
        assert_eq!(
            padding(tiling, CLIENT_FRAME_INSET),
            Edges {
                top: px(0.0),
                left: px(0.0),
                bottom: px(CLIENT_FRAME_INSET),
                right: px(CLIENT_FRAME_INSET)
            }
        );
        let regions = resize_regions(viewport, tiling, CLIENT_FRAME_INSET);
        assert!(regions.iter().all(|(_, edge)| matches!(
            edge,
            ResizeEdge::Right | ResizeEdge::Bottom | ResizeEdge::BottomRight
        )));
        assert!(resize_regions(viewport, Tiling::tiled(), CLIENT_FRAME_INSET).is_empty());
        let regions = resize_regions(viewport, Tiling::default(), CLIENT_FRAME_INSET);
        let edge_at = |position| {
            regions
                .iter()
                .rev()
                .find(|(bounds, _)| bounds.contains(&position))
                .map(|(_, edge)| *edge)
        };
        assert_eq!(
            edge_at(point(
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0),
                px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0)
            )),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(
            edge_at(point(px(500.0), px(CLIENT_FRAME_INSET - RESIZE_BAND / 2.0))),
            Some(ResizeEdge::Top)
        );
        assert_eq!(
            edge_at(point(
                px(900.0 + CLIENT_FRAME_INSET + RESIZE_BAND / 2.0),
                px(580.0 + CLIENT_FRAME_INSET + RESIZE_BAND / 2.0)
            )),
            Some(ResizeEdge::BottomRight)
        );
        assert_eq!(
            edge_at(point(
                px(CLIENT_FRAME_INSET + 1.0),
                px(CLIENT_FRAME_INSET + 1.0)
            )),
            None
        );
        assert_eq!(edge_at(point(px(2.0), px(2.0))), None);
    }
}
