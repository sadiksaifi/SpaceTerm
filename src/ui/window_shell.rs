//! A client frame around application content, driven by the window's decoration facts.

use gpui::{
    AnyElement, App, Bounds, BoxShadow, ClientFrame, Corners, CursorStyle, Decorations, Edges,
    HitboxBehavior, MouseButton, MouseDownEvent, Pixels, ResizeEdge, Size, Tiling, Window, canvas,
    div, point, prelude::*, px, rgba, size,
};

use crate::platform::window_chrome::CLIENT_FRAME_INSET;

const RESIZE_BAND: f32 = 10.0;
const CORNER_TARGET: f32 = 24.0;

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

pub(super) fn render(content: impl IntoElement, window: &mut Window, cx: &mut App) -> AnyElement {
    window.use_keyed_state("client-window-frame-observer", cx, |window, _| {
        window.observe_window_appearance(|window, _| window.refresh())
    });
    let Decorations::Client { tiling } = window.window_decorations() else {
        window.set_client_inset(px(0.0));
        window.set_client_frame(None);
        window.set_input_region(None);
        return content.into_any_element();
    };
    let tiling = if window.is_fullscreen() || window.is_maximized() {
        Tiling::tiled()
    } else {
        tiling
    };
    let transparent_frame = window.supports_transparent_client_frame();
    let frame_inset = if transparent_frame {
        CLIENT_FRAME_INSET
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
            corner_radius: px(16.0),
            tiling,
            shadows: vec![BoxShadow {
                color: rgba(if active { 0x00000060 } else { 0x00000030 }).into(),
                offset: point(px(0.0), px(5.0)),
                blur_radius: px(20.0),
                spread_radius: px(0.0),
                inset: false,
            }]
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
    window.set_input_region(Some(&[Bounds::new(
        point(input_left, input_top),
        size(
            viewport.width - input_left - input_right,
            viewport.height - input_top - input_bottom,
        ),
    )]));
    let radius = |a: bool, b: bool| {
        px(if a || b || !transparent_frame {
            0.0
        } else {
            16.0
        })
    };
    let radii = Corners {
        top_left: radius(tiling.top, tiling.left),
        top_right: radius(tiling.top, tiling.right),
        bottom_left: radius(tiling.bottom, tiling.left),
        bottom_right: radius(tiling.bottom, tiling.right),
    };
    let edge = super::appearance::gpui_color(super::appearance::chrome(cx).colors.border);
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
                    .border_1()
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
                                (window.insert_hitbox(region, HitboxBehavior::Normal), edge)
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
            point(px(20.0), px(20.0)),
            MouseButton::Left,
            gpui::Modifiers::none(),
        );
        cx.simulate_mouse_up(
            point(px(20.0), px(20.0)),
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
        cx.simulate_click(point(px(20.0), px(20.0)), gpui::Modifiers::none());
        assert_eq!(cx.window_requests().len(), 1);
    }

    #[test]
    fn tiled_edges_have_neither_insets_nor_resize_targets() {
        let viewport = size(px(948.0), px(628.0));
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
                bottom: px(24.0),
                right: px(24.0)
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
            edge_at(point(px(20.0), px(20.0))),
            Some(ResizeEdge::TopLeft)
        );
        assert_eq!(edge_at(point(px(500.0), px(20.0))), Some(ResizeEdge::Top));
        assert_eq!(
            edge_at(point(px(930.0), px(610.0))),
            Some(ResizeEdge::BottomRight)
        );
        assert_eq!(edge_at(point(px(25.0), px(25.0))), None);
        assert_eq!(edge_at(point(px(2.0), px(2.0))), None);
    }
}
