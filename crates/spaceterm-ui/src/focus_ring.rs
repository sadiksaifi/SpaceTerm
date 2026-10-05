//! The keyboard focus ring every focusable control draws around itself.
//!
//! One band, measured from AppKit on macOS: its outer edge sits 3pt outside the control's fill and
//! it runs 3.5pt inward, so it overlaps the fill by half a point and covers any border drawn around
//! the fill. Its corners stay concentric with the control's. The ring wraps its control and paints
//! after it, and the control's border fades out as the band fades in, so neither the border nor
//! anything else the control draws shows through the translucent band. The control otherwise keeps
//! its resting appearance; the ring alone states focus. On gaining focus the band starts wide
//! and far out, faint, and contracts onto the control while it fades in. On losing focus it
//! disappears at once.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Corners, Element, ElementId, FocusHandle,
    GlobalElementId, InspectorElementId, Interactivity, LayoutId, Pixels, Rgba, StyleRefinement,
    Window, div, px,
};

use crate::ControlMotion;

/// How far the ring's outer edge sits beyond the control's fill.
const REACH: f32 = 3.0;
/// The band's width, which covers the control's border and half a point of its fill.
const WIDTH: f32 = 3.5;
/// How much wider and further out the band starts when it appears.
const ENTRANCE_SPREAD: f32 = 17.0;
pub(crate) const ENTRANCE: Duration = Duration::from_millis(250);

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
        corner_radii: Corners::all(corner_radius),
        border_width,
        visibility: Visibility::Shown,
        target: None,
        debug_selector: None,
        outline: None,
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
    corner_radii: Corners<Pixels>,
    border_width: Pixels,
    visibility: Visibility,
    target: Option<FocusRingTarget>,
    debug_selector: Option<String>,
    outline: Option<(Pixels, Pixels)>,
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
    /// Paints a desktop outline with its own geometry and a simple native fade.
    pub(crate) fn outline(mut self, width: Pixels, outset: Pixels, radius: Pixels) -> Self {
        self.outline = Some((width, outset));
        self.corner_radii = Corners::all(radius);
        self
    }

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

    /// Gives each corner its own outer radius, for a control whose corners differ, such as a
    /// segment joined to its neighbor.
    pub(crate) fn corner_radii(mut self, radii: Corners<Pixels>) -> Self {
        self.corner_radii = radii;
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

    /// The band's resting bounds and outer corner radii around a control's border box.
    fn resting(&self, control: Bounds<Pixels>) -> (Bounds<Pixels>, Corners<Pixels>) {
        if let Some((_, outset)) = self.outline {
            return (
                control.dilate(outset),
                self.corner_radii.map(|radius| *radius + outset),
            );
        }
        let reach = px(REACH) - self.border_width;
        let radii = self
            .corner_radii
            .map(|radius| (*radius - self.border_width).max(px(0.0)) + px(REACH));
        (control.dilate(reach), radii)
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

impl<E: IntoElement + Styled + 'static> IntoElement for Ringed<E> {
    type Element = RingedElement;

    fn into_element(self) -> Self::Element {
        let mut host = self.host;
        RingedElement {
            build: Some(Box::new(move |cover| {
                fade_border(host.style(), cover);
                host.into_any_element()
            })),
            host: None,
            ring: self.ring,
            marker: None,
        }
    }
}

/// Takes `cover` of the host's border away, where `cover` is how opaque the band over it is.
///
/// The band is translucent, so painting it over the border would still show the border through it.
fn fade_border(style: &mut StyleRefinement, cover: f32) {
    if cover <= 0.0 {
        return;
    }
    if let Some(border) = style.border_color.as_mut() {
        border.a *= 1.0 - cover.min(1.0);
    }
}

/// The element a [`Ringed`] control becomes.
pub struct RingedElement {
    /// Builds the control once layout knows how much of its border the band covers.
    build: Option<Box<dyn FnOnce(f32) -> AnyElement>>,
    host: Option<AnyElement>,
    ring: Option<FocusRing>,
    /// An empty element over the ring's resting bounds, which carries its debug selector.
    marker: Option<AnyElement>,
}

/// When the ring became visible, retained across frames by the ring's id.
#[derive(Default)]
struct BandState {
    shown_since: Option<Instant>,
}

impl RingedElement {
    fn host_mut(&mut self) -> &mut AnyElement {
        self.host
            .as_mut()
            .expect("the control is built during layout")
    }
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
        if let Some(build) = self.build.take() {
            let cover = self
                .ring
                .as_ref()
                .map_or(0.0, |ring| ring.border_cover(window, cx));
            self.host = Some(build(cover));
        }
        (self.host_mut().request_layout(window, cx), ())
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
        self.host_mut().prepaint(window, cx);
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
        self.host_mut().paint(window, cx);
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

    /// How opaque the band over the control's border is this frame, from 0 to 1.
    ///
    /// A ring around one part of the control leaves the control's border alone, and a ring whose
    /// color is invisible, as in an inactive window, covers nothing.
    fn border_cover(&self, window: &mut Window, cx: &mut App) -> f32 {
        if self.target.is_some() || self.color.a <= 0.0 {
            return 0.0;
        }
        self.progress(window, cx)
            .map_or(0.0, |progress| Band::at(progress).opacity)
    }

    /// How far the entrance has run, or `None` while the ring is hidden.
    ///
    /// Layout and paint both read it in one frame; the first reading that finds the ring shown
    /// starts the entrance.
    fn progress(&self, window: &mut Window, cx: &mut App) -> Option<f32> {
        let shown = match &self.visibility {
            Visibility::Shown => true,
            Visibility::Tracking { focus, pinned } => *pinned || focus.is_focused(window),
        };
        let pinned = matches!(self.visibility, Visibility::Tracking { pinned: true, .. });
        let now = cx.background_executor().now();
        let motion = crate::control_motion(cx);
        window.with_global_id(self.id.clone(), |id, window| {
            window.with_element_state(id, |state: Option<BandState>, _| {
                let mut state = state.unwrap_or_default();
                state.shown_since = shown.then(|| state.shown_since.unwrap_or(now));
                let progress = state.shown_since.map(|since| {
                    if motion == ControlMotion::Reduced || pinned {
                        1.0
                    } else {
                        let elapsed = now.saturating_duration_since(since);
                        if self.outline.is_some() {
                            (elapsed.as_secs_f32() / 0.2).min(1.0)
                        } else {
                            entrance_progress(elapsed)
                        }
                    }
                });
                (progress, state)
            })
        })
    }

    fn paint(&self, host: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let Some(progress) = self.progress(window, cx) else {
            return;
        };
        if progress < 1.0 {
            window.request_animation_frame();
        }
        let band = if self.outline.is_some() {
            Band {
                spread: 0.0,
                opacity: progress,
            }
        } else {
            Band::at(progress)
        };
        let color = Rgba {
            a: self.color.a * band.opacity,
            ..self.color
        };
        if color.a <= 0.0 {
            return;
        }
        let (resting, radii) = self.resting(self.control_bounds(host));
        let spread = px(band.spread);
        window.paint_quad(gpui::quad(
            resting.dilate(spread),
            radii.map(|radius| *radius + spread),
            gpui::transparent_black(),
            self.outline.map_or(px(WIDTH), |(width, _)| width) + spread,
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
    fn desktop_focus_outline_stays_inside_the_native_pointer_target() {
        let control = Bounds::new(
            gpui::point(px(20.0), px(30.0)),
            gpui::size(px(34.0), px(34.0)),
        );
        let ring = focus_ring("native", RING, px(12.0), px(0.0)).outline(px(2.0), px(0.0), px(9.0));
        assert_eq!(ring.resting(control), (control, Corners::all(px(9.0))));
    }

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
        cx.update(|cx| {
            {
                crate::init(
                    cx,
                    Box::new(catalog.clone()),
                    Box::new(catalog.clone()),
                    Box::new(catalog.clone()),
                    Box::new(catalog),
                )
            }
            .unwrap()
        });
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
    fn band_hides_its_control_border_and_paints_over_it(cx: &mut gpui::TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| Fixture {
            focus: cx.focus_handle(),
            border_width: px(1.0),
        });
        let border = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, _| {
                window
                    .painted_quads()
                    .into_iter()
                    .filter(|quad| {
                        let color = Rgba::from(quad.border_color);
                        (color.r - BORDER.r).abs() < 0.01 && (color.b - BORDER.b).abs() < 0.01
                    })
                    .map(|quad| (quad.order, quad.border_color.a))
                    .collect::<Vec<_>>()
            })
        };
        cx.update(|window, cx| root.read(cx).focus.clone().focus(window, cx));
        cx.run_until_parked();
        cx.executor().advance_clock(ENTRANCE / 2);
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();

        let entering = border(cx);
        let band = ring_quads(cx)[0].order;
        assert!(!entering.is_empty(), "the border is still fading out");
        for (order, alpha) in entering {
            assert!(
                alpha > 0.0 && alpha < 1.0,
                "the border fades, alpha {alpha}"
            );
            assert!(band > order, "the band paints over the border");
        }

        settle(cx);
        assert!(
            border(cx).iter().all(|(_, alpha)| *alpha == 0.0),
            "no border shows through the translucent band at rest"
        );
    }
}
