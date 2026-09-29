//! The keyboard focus ring every focusable control draws around itself.
//!
//! One band, measured from AppKit on macOS: it starts 2pt outside the control's edge and runs
//! 3.5pt inward, so it covers the control's own border rather than floating beside it. Its corners
//! stay concentric with the control's. The control keeps its resting border underneath; the ring
//! alone states focus. On gaining focus the band starts wide and far out, faint, and contracts onto
//! the control while it fades in. On losing focus it disappears at once.
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    App, Bounds, Div, Element, ElementId, FocusHandle, GlobalElementId, InspectorElementId,
    LayoutId, Pixels, Rgba, Window, div, px,
};

use crate::ControlMotion;

/// How far the ring's outer edge sits beyond the control's outer edge.
const OUTSET: f32 = 2.0;
/// The band's width, which covers the control's border and a little of its interior.
const WIDTH: f32 = 3.5;
/// How much wider and further out the band starts when it appears.
const ENTRANCE_SPREAD: f32 = 17.0;
const ENTRANCE: Duration = Duration::from_millis(250);

/// Creates the ring for a control that already knows whether it has keyboard focus.
///
/// Attach it as a child of the control and only while the ring should show: mounting it starts
/// the entrance, and unmounting it removes the ring at once.
pub fn focus_ring(
    id: impl Into<ElementId>,
    color: Rgba,
    corner_radius: Pixels,
    border_width: Pixels,
) -> FocusRing {
    FocusRing {
        id: id.into(),
        color,
        corner_radius,
        border_width,
        visibility: Visibility::Shown,
        debug_selector: None,
    }
}

/// A keyboard focus ring positioned from its control's outer geometry.
pub struct FocusRing {
    id: ElementId,
    color: Rgba,
    corner_radius: Pixels,
    border_width: Pixels,
    visibility: Visibility,
    debug_selector: Option<String>,
}

#[derive(Clone)]
enum Visibility {
    Shown,
    /// Resolved while painting, so a caller that builds its layout before it knows which element
    /// is focused still gets a ring that follows the handle.
    Tracking {
        focus: FocusHandle,
        pinned: bool,
    },
}

impl FocusRing {
    /// Shows the ring only while `focus` is the window's focused handle.
    ///
    /// `pinned` keeps the ring at rest regardless of focus, for development previews.
    pub(crate) fn tracking(mut self, focus: &FocusHandle, pinned: bool) -> Self {
        self.visibility = Visibility::Tracking {
            focus: focus.clone(),
            pinned,
        };
        self
    }

    /// Names the ring's resting bounds for layout tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }
}

impl IntoElement for FocusRing {
    type Element = Div;

    fn into_element(self) -> Self::Element {
        // Absolute insets resolve against the control's padding box, so the control's own border
        // is added back to reach its outer edge.
        let offset = px(OUTSET) + self.border_width;
        let selector = self.debug_selector;
        div()
            .absolute()
            .top(-offset)
            .right(-offset)
            .bottom(-offset)
            .left(-offset)
            .when_some(selector, |ring, selector| {
                ring.debug_selector(move || selector)
            })
            .child(FocusRingBand {
                id: self.id,
                color: self.color,
                corner_radius: self.corner_radius,
                visibility: self.visibility,
            })
    }
}

/// Paints the band inside the ring's resting bounds, which the animation may extend past.
struct FocusRingBand {
    id: ElementId,
    color: Rgba,
    corner_radius: Pixels,
    visibility: Visibility,
}

/// When the ring became visible, retained across frames by the band's element id.
#[derive(Default)]
struct BandState {
    shown_since: Option<Instant>,
}

impl IntoElement for FocusRingBand {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for FocusRingBand {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
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
        let style = gpui::Style {
            size: gpui::size(gpui::relative(1.0), gpui::relative(1.0)).map(Into::into),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let shown = match &self.visibility {
            Visibility::Shown => true,
            Visibility::Tracking { focus, pinned } => *pinned || focus.is_focused(window),
        };
        let now = cx.background_executor().now();
        let motion = crate::control_theme_catalog(cx)
            .map_or_else(ControlMotion::default, |c| c.installed_motion());
        let pinned = matches!(self.visibility, Visibility::Tracking { pinned: true, .. });
        let Some(id) = id else {
            return;
        };
        let progress = window.with_element_state(id, |state: Option<BandState>, _| {
            let mut state = state.unwrap_or_default();
            state.shown_since = shown.then(|| state.shown_since.unwrap_or(now));
            let progress = state.shown_since.map(|since| {
                if motion == ControlMotion::Reduced || pinned {
                    1.0
                } else {
                    entrance_progress(now.saturating_duration_since(since))
                }
            });
            (progress, state)
        });
        let Some(progress) = progress else {
            return;
        };
        if progress < 1.0 {
            window.request_animation_frame();
        }
        let band = Band::at(progress);
        let color = Rgba {
            a: self.color.a * band.opacity,
            ..self.color
        };
        if color.a <= 0.0 {
            return;
        }
        let spread = px(band.spread);
        window.paint_quad(gpui::quad(
            bounds.dilate(spread),
            self.corner_radius + px(OUTSET) + spread,
            gpui::transparent_black(),
            px(WIDTH) + spread,
            color,
            gpui::BorderStyle::Solid,
        ));
    }
}

/// Finishes every ring entrance in progress, since test windows have no frame loop.
#[cfg(test)]
pub(crate) fn settle(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(ENTRANCE);
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

fn entrance_progress(elapsed: Duration) -> f32 {
    (elapsed.as_secs_f32() / ENTRANCE.as_secs_f32()).min(1.0)
}

/// The band's shape at one point of its entrance.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Band {
    /// How far the band's outer edge extends past its resting position, in points. The inner edge
    /// never moves, so the band is this much wider as well.
    spread: f32,
    /// The fraction of the ring color's own opacity the band paints with.
    opacity: f32,
}

impl Band {
    /// The contraction decelerates harder than the fade, as AppKit's does: the band is mostly in
    /// place while it is still visibly brightening.
    fn at(progress: f32) -> Self {
        let remaining = 1.0 - progress.clamp(0.0, 1.0);
        Self {
            spread: ENTRANCE_SPREAD * remaining.powi(3),
            opacity: 1.0 - remaining.powi(2),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrance_contracts_onto_the_control_and_fades_in() {
        assert_eq!(
            Band::at(0.0),
            Band {
                spread: ENTRANCE_SPREAD,
                opacity: 0.0
            }
        );
        assert_eq!(
            Band::at(1.0),
            Band {
                spread: 0.0,
                opacity: 1.0
            }
        );
        let samples = [0.1, 0.3, 0.5, 0.7, 0.9].map(Band::at);
        for pair in samples.windows(2) {
            assert!(pair[1].spread < pair[0].spread && pair[1].opacity > pair[0].opacity);
        }
        assert_eq!(entrance_progress(ENTRANCE * 2), 1.0);
    }

    struct Fixture {
        focus: FocusHandle,
        border_width: Pixels,
    }

    impl gpui::Render for Fixture {
        fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
            div().size_full().p(px(40.0)).child(
                div()
                    .debug_selector(|| "control".to_owned())
                    .relative()
                    .w(px(200.0))
                    .h(px(24.0))
                    .rounded(px(6.0))
                    .border(self.border_width)
                    .child(
                        focus_ring("ring", RING, px(6.0), self.border_width)
                            .tracking(&self.focus, false)
                            .debug_selector("control-ring"),
                    ),
            )
        }
    }

    const RING: Rgba = Rgba {
        r: 0.1,
        g: 0.66,
        b: 1.0,
        a: 0.5,
    };

    /// The ring bands painted, one per band. The window paints a border-only quad as strips
    /// that share the band's bounds.
    fn ring_quads(cx: &mut gpui::VisualTestContext) -> Vec<gpui::Quad> {
        cx.update(|window, _| {
            let mut bands: Vec<gpui::Quad> = Vec::new();
            for quad in window.painted_quads() {
                let color = Rgba::from(quad.border_color);
                let is_ring = (color.b - RING.b).abs() < 0.01 && (color.r - RING.r).abs() < 0.01;
                if is_ring && !bands.iter().any(|band| band.bounds == quad.bounds) {
                    bands.push(quad);
                }
            }
            bands
        })
    }

    fn scaled(
        bounds: Bounds<Pixels>,
        cx: &mut gpui::VisualTestContext,
    ) -> Bounds<gpui::ScaledPixels> {
        let scale = cx.update(|window, _| window.scale_factor());
        bounds.scale(scale)
    }

    #[gpui::test]
    fn ring_is_concentric_with_its_control_and_covers_its_border(cx: &mut gpui::TestAppContext) {
        for border_width in [px(0.0), px(1.0), px(2.0)] {
            let (root, cx) = cx.add_window_view(|_, cx| Fixture {
                focus: cx.focus_handle(),
                border_width,
            });
            cx.run_until_parked();
            assert!(
                ring_quads(cx).is_empty(),
                "an unfocused control paints no ring"
            );

            cx.update(|window, cx| root.read(cx).focus.clone().focus(window, cx));
            settle(cx);

            let control = cx.debug_bounds("control").unwrap();
            let resting = cx.debug_bounds("control-ring").unwrap();
            assert_eq!(
                resting,
                control.dilate(px(OUTSET)),
                "border {border_width:?}"
            );
            let scale = cx.update(|window, _| window.scale_factor());
            let quads = ring_quads(cx);
            let [ring] = quads.as_slice() else {
                panic!("expected one ring quad, painted {quads:?}");
            };
            assert_eq!(ring.bounds, scaled(resting, cx));
            assert_eq!(ring.corner_radii.top_left, px(6.0 + OUTSET).scale(scale));
            assert_eq!(ring.border_widths.left, px(WIDTH).scale(scale));
            assert!((Rgba::from(ring.border_color).a - RING.a).abs() < 0.001);
            assert!(ring.background.is_transparent());
        }
    }

    #[gpui::test]
    fn ring_enters_from_outside_and_leaves_at_once(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| Fixture {
            focus: cx.focus_handle(),
            border_width: px(1.0),
        });
        cx.update(|window, cx| root.read(cx).focus.clone().focus(window, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(ENTRANCE / 5);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
        let resting = cx.debug_bounds("control-ring").unwrap();
        let resting = scaled(resting, cx);
        let entering = ring_quads(cx);
        let [entering] = entering.as_slice() else {
            panic!("expected the entering ring, painted {entering:?}");
        };
        assert!(entering.bounds.contains(&resting.origin));
        assert!(entering.bounds.size.width > resting.size.width);
        assert!(Rgba::from(entering.border_color).a < RING.a);

        settle(cx);
        assert_eq!(ring_quads(cx)[0].bounds, resting);

        cx.update(|window, cx| window.blur(cx));
        cx.run_until_parked();
        assert!(
            ring_quads(cx).is_empty(),
            "blur removes the ring without an exit"
        );
    }

    #[gpui::test]
    fn reduced_motion_shows_the_ring_at_rest(cx: &mut gpui::TestAppContext) {
        let catalog = crate::catalog_tests::catalog(1).motion(ControlMotion::Reduced);
        cx.update(|cx| crate::init(cx, catalog).unwrap());
        let (root, cx) = cx.add_window_view(|_, cx| Fixture {
            focus: cx.focus_handle(),
            border_width: px(1.0),
        });
        cx.update(|window, cx| root.read(cx).focus.clone().focus(window, cx));
        cx.run_until_parked();
        let resting = cx.debug_bounds("control-ring").unwrap();
        let resting = scaled(resting, cx);
        let quads = ring_quads(cx);
        let [ring] = quads.as_slice() else {
            panic!("expected the ring at rest, painted {quads:?}");
        };
        assert_eq!(ring.bounds, resting);
        assert!((Rgba::from(ring.border_color).a - RING.a).abs() < 0.001);
    }
}
