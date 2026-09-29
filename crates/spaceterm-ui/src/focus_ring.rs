//! The keyboard focus ring every focusable control draws around itself.
//!
//! One band, measured from AppKit on macOS: its outer edge sits 3pt outside the control's fill and
//! it runs 3.5pt inward, so it overlaps the fill by half a point and covers any border drawn around
//! the fill. Its corners stay concentric with the control's. The ring wraps its control and paints
//! after it, so nothing the control draws, its border included, shows through the band. The control
//! keeps its resting appearance; the ring alone states focus. On gaining focus the band starts wide
//! and far out, faint, and contracts onto the control while it fades in. On losing focus it
//! disappears at once.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Element, ElementId, FocusHandle, GlobalElementId,
    InspectorElementId, Interactivity, LayoutId, Pixels, Rgba, StyleRefinement, Window, div, px,
};

use crate::ControlMotion;

/// How far the ring's outer edge sits beyond the control's fill.
const REACH: f32 = 3.0;
/// The band's width, which covers the control's border and half a point of its fill.
const WIDTH: f32 = 3.5;
/// How much wider and further out the band starts when it appears.
const ENTRANCE_SPREAD: f32 = 17.0;
const ENTRANCE: Duration = Duration::from_millis(250);

/// Creates the ring for a control with the given outer corner radius and border width.
///
/// Wrap the control with [`FocusRing::around`] only while the ring should show: mounting it starts
/// the entrance, and unmounting it removes the ring at once. The id must be unique among the
/// control's siblings; derive it from the control's own id.
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
        target: None,
        debug_selector: None,
    }
}

/// The ring id for a control with the given id, unique wherever the control's id is.
pub(crate) fn ring_id(control: &ElementId) -> ElementId {
    ElementId::NamedChild(std::sync::Arc::new(control.clone()), "focus-ring".into())
}

/// A keyboard focus ring positioned from its control's geometry.
pub struct FocusRing {
    id: ElementId,
    color: Rgba,
    corner_radius: Pixels,
    border_width: Pixels,
    visibility: Visibility,
    target: Option<FocusRingTarget>,
    debug_selector: Option<String>,
}

/// Bounds a wrapped control reports while it prepaints, for a ring around one of its parts.
pub(crate) type FocusRingTarget = Rc<Cell<Option<Bounds<Pixels>>>>;

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

    /// Surrounds the part of the wrapped control whose bounds `target` holds after the control
    /// prepaints, instead of the whole control. Without bounds the ring surrounds the control.
    pub(crate) fn target(mut self, target: FocusRingTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Names the ring's resting bounds for layout tests.
    pub fn debug_selector(mut self, selector: impl Into<String>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    /// Wraps `host` so the ring paints after everything the host paints.
    pub fn around<E>(self, host: E) -> Ringed<E> {
        Ringed::new(host, Some(self))
    }

    /// The band's resting bounds and outer corner radius around a control's border box.
    fn resting(&self, control: Bounds<Pixels>) -> (Bounds<Pixels>, Pixels) {
        let reach = px(REACH) - self.border_width;
        let inner_radius = (self.corner_radius - self.border_width).max(px(0.0));
        (control.dilate(reach), inner_radius + px(REACH))
    }
}

/// A control and the focus ring that may surround it.
///
/// It lays out exactly as the control does and forwards the control's builder traits, so wrapping
/// changes neither the control's layout nor its element ids.
pub struct Ringed<E> {
    host: E,
    ring: Option<FocusRing>,
}

impl<E> Ringed<E> {
    /// Wraps `host` with a ring that may be absent, for a control whose ring comes and goes.
    pub fn new(host: E, ring: Option<FocusRing>) -> Self {
        Self { host, ring }
    }
}

impl<E: Styled> Styled for Ringed<E> {
    fn style(&mut self) -> &mut StyleRefinement {
        self.host.style()
    }
}

impl<E: ParentElement> ParentElement for Ringed<E> {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.host.extend(elements);
    }
}

impl<E: InteractiveElement> InteractiveElement for Ringed<E> {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.host.interactivity()
    }
}

impl<E: StatefulInteractiveElement> StatefulInteractiveElement for Ringed<E> {}

impl<E: IntoElement> IntoElement for Ringed<E> {
    type Element = RingedElement;

    fn into_element(self) -> Self::Element {
        RingedElement {
            host: self.host.into_any_element(),
            ring: self.ring,
            marker: None,
        }
    }
}

/// The element a [`Ringed`] control becomes.
pub struct RingedElement {
    host: AnyElement,
    ring: Option<FocusRing>,
    /// An empty element over the ring's resting bounds, which carries its debug selector.
    marker: Option<AnyElement>,
}

/// When the ring became visible, retained across frames by the ring's id.
#[derive(Default)]
struct BandState {
    shown_since: Option<Instant>,
}

impl IntoElement for RingedElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for RingedElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        // The ring keys its own state, so the control keeps the ids it has without a ring.
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
        (self.host.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.host.prepaint(window, cx);
        let Some(ring) = &self.ring else {
            return;
        };
        let Some(selector) = ring.debug_selector.clone() else {
            return;
        };
        let (resting, _) = ring.resting(ring.control_bounds(bounds));
        let mut marker = div()
            .w(resting.size.width)
            .h(resting.size.height)
            .debug_selector(move || selector)
            .into_any_element();
        marker.layout_as_root(AvailableSpace::min_size(), window, cx);
        marker.prepaint_at(resting.origin, window, cx);
        self.marker = Some(marker);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.host.paint(window, cx);
        if let Some(marker) = &mut self.marker {
            marker.paint(window, cx);
        }
        if let Some(ring) = &self.ring {
            ring.paint(bounds, window, cx);
        }
    }
}

impl FocusRing {
    fn control_bounds(&self, host: Bounds<Pixels>) -> Bounds<Pixels> {
        self.target
            .as_ref()
            .and_then(|target| target.get())
            .unwrap_or(host)
    }

    fn paint(&self, host: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let shown = match &self.visibility {
            Visibility::Shown => true,
            Visibility::Tracking { focus, pinned } => *pinned || focus.is_focused(window),
        };
        let pinned = matches!(self.visibility, Visibility::Tracking { pinned: true, .. });
        let now = cx.background_executor().now();
        let motion = crate::control_motion(cx);
        let progress = window.with_global_id(self.id.clone(), |id, window| {
            window.with_element_state(id, |state: Option<BandState>, _| {
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
            })
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
        let (resting, radius) = self.resting(self.control_bounds(host));
        let spread = px(band.spread);
        window.paint_quad(gpui::quad(
            resting.dilate(spread),
            radius + spread,
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
                focus_ring("ring", RING, px(6.0), self.border_width)
                    .tracking(&self.focus, false)
                    .debug_selector("control-ring")
                    .around(
                        div()
                            .debug_selector(|| "control".to_owned())
                            .w(px(200.0))
                            .h(px(24.0))
                            .rounded(px(6.0))
                            .border(self.border_width)
                            .border_color(BORDER),
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

    const BORDER: Rgba = Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
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

            // The outer edge sits a fixed reach outside the fill, which the border insets.
            let control = cx.debug_bounds("control").unwrap();
            let resting = cx.debug_bounds("control-ring").unwrap();
            assert_eq!(
                resting,
                control.dilate(px(REACH) - border_width),
                "border {border_width:?}"
            );
            let scale = cx.update(|window, _| window.scale_factor());
            let quads = ring_quads(cx);
            let [ring] = quads.as_slice() else {
                panic!("expected one ring quad, painted {quads:?}");
            };
            assert_eq!(ring.bounds, scaled(resting, cx));
            assert_eq!(
                ring.corner_radii.top_left,
                (px(6.0) - border_width + px(REACH)).scale(scale)
            );
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
        let catalog = crate::catalog_tests::catalog_with_motion(1, ControlMotion::Reduced);
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

    #[gpui::test]
    fn ring_paints_over_its_control_border(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| Fixture {
            focus: cx.focus_handle(),
            border_width: px(1.0),
        });
        cx.update(|window, cx| root.read(cx).focus.clone().focus(window, cx));
        settle(cx);

        let ring = ring_quads(cx)[0].order;
        let border = cx.update(|window, _| {
            window
                .painted_quads()
                .into_iter()
                .filter(|quad| {
                    let color = Rgba::from(quad.border_color);
                    (color.r - BORDER.r).abs() < 0.01 && (color.b - BORDER.b).abs() < 0.01
                })
                .map(|quad| quad.order)
                .max()
                .expect("the control paints its border")
        });
        assert!(ring > border, "the band must cover the control's border");
    }
}
