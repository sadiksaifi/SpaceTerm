//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts, and the
//! motion curve their layout changes share.
//! A [`DragSession`] ties owner state to the one GPUI drag it started, so an unseen release cannot
//! strand it.

use gpui::prelude::*;
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Along as _, AnyElement, App, AvailableSpace, Axis, Bounds, BoxShadow, ContentMask, Context,
    DispatchPhase, Div, Element, ElementId, GlobalElementId, InspectorElementId, KeystrokeEvent,
    LayoutId, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollHandle, Size, Style,
    Subscription, Window, WindowId, canvas, div, point, px,
};

use super::appearance::gpui_color;

/// How long the other items take to slide aside as the landing slot moves.
const SHIFT_DURATION: Duration = Duration::from_millis(310);
/// How long a released item takes to settle into its slot, and the others to follow it.
const SETTLE_DURATION: Duration = Duration::from_millis(440);
/// How long a new item takes to grow into the strip, and the gap a removed item leaves to close.
pub(super) const PRESENCE_DURATION: Duration = Duration::from_millis(440);
/// How long the selection takes to move from one item to another that holds still.
const SELECTION_DURATION: Duration = Duration::from_millis(310);
/// The longest a selection keeps gliding after an item that keeps moving, before it lands on it.
const SELECTION_LIMIT: Duration = Duration::from_millis(1000);
/// How long a removed item, and the selection it held, take to fade where they stood.
pub(super) const LEAVING_FADE: Duration = Duration::from_millis(100);
/// How long the place a removed item leaves takes to close, from the moment it leaves: its fade
/// and then [`PRESENCE_DURATION`]. The place starts closing slowly, so it moves at once yet takes
/// less than 3% of itself while the item fades in it, and the item fades whole.
pub(super) const CLOSING_DURATION: Duration = Duration::from_millis(540);
/// How much more of its growth an item makes after a part of it first shows whole, while that
/// part fades in.
const REVEAL_SPAN: f32 = 0.08;
/// How deep into either end of a strip the pointer starts scrolling it.
const AUTOSCROLL_EDGE: f32 = 40.0;
/// How fast a strip scrolls, in points per second, with the pointer at or past its end.
const AUTOSCROLL_SPEED: f32 = 720.0;
/// The longest step one autoscroll frame takes, so a stalled frame does not jump the strip.
const AUTOSCROLL_STEP: Duration = Duration::from_millis(50);
/// How opaque a lifted item's face is where a release would land it, so it reads as a preview
/// of the drop rather than the item itself.
const PLACEHOLDER_OPACITY: f32 = 0.4;
/// How opaque a lifted item is, so the items it passes over show through a little.
pub(crate) const LIFTED_OPACITY: f32 = 0.95;

/// The drag state of one scrolling strip of items laid out along one axis.
///
/// While an item is lifted, its lifted copy follows the pointer inside the strip and the other items
/// slide aside to open a gap where a release would land it. A faded face of the item moves into
/// that gap. A release anywhere settles the item into that place; a cancellation returns everything
/// to rest.
///
/// The selection rests on one `Row` of an item. A strip whose items are single rows uses `()`.
pub(crate) struct ReorderableStrip<Id, Row = ()> {
    axis: Axis,
    /// Whether a dragged item can join another item, so the pointer rather than the lifted
    /// copy's center decides where it lands. Past either end the copy stays held over the end
    /// item, where only the pointer still tells joining it from landing beyond it.
    joinable: bool,
    lift: Option<Lift<Id>>,
    /// Each item's displacement from its laid-out place, as it eases toward its target.
    slides: HashMap<Id, Slide>,
    /// The released item while it settles, which still shows its lift.
    settling: Option<Id>,
    /// The strip's items at the last refresh, in presentation order, or `None` before the first.
    known: Option<Vec<Id>>,
    /// Each new item's natural size along the axis, once measured, as it grows into the strip.
    entering: HashMap<Id, (Ease, Option<Pixels>)>,
    /// The items that finished growing at the last refresh, and take their full size this frame.
    grown: Vec<Id>,
    /// The new items that reach past the strip's visible end, where the strip holds them while
    /// they grow, so their faces grow from that end.
    growing_from_end: HashSet<Id>,
    /// The closing gap a removed item left before each item, inside that item's slot.
    leading_gaps: HashMap<Id, Gap<Id>>,
    /// The closing gap a removed last item left after the item that is now last, inside that
    /// item's slot.
    trailing_gaps: HashMap<Id, Gap<Id>>,
    /// When the strip last prepared a frame.
    refreshed_at: Option<Instant>,
    /// Where each item of the last frame started along the strip, and its size, in layout
    /// coordinates.
    laid_out: HashMap<Id, (Pixels, Pixels)>,
    /// When the strip prepared the frame `laid_out` comes from.
    laid_out_at: Option<Instant>,
    /// The selected item and row at the last refresh.
    selected: Option<(Id, Row)>,
    /// Where the strip's [`SelectionOverlay`] drew the selection in the last frame, so the
    /// selection glides on from there.
    drawn_selection: Rc<Cell<Option<DrawnSelection>>>,
    /// The selection moving to the selected row.
    flight: Option<Flight<Id, Row>>,
    /// The selection a removed item held, where it was last drawn, and when the item left.
    departing_selection: Vec<DepartingSelection>,
    /// How far the strip was scrolled along its axis in the last frame.
    scrolled: Pixels,
    /// Whether the items of the last frame ran past the strip's visible end.
    overflowing: bool,
    /// The size each item takes along the strip when the strip has room for it, for a strip whose
    /// items narrow together as it fills. New items grow toward it.
    natural_extent: Option<Pixels>,
    /// Whether the items of the last frame stood narrower than [`Self::natural_extent`], because
    /// the strip had no room left for them.
    squeezed: bool,
    /// Where the strip's visible part ended in the last frame, in layout coordinates.
    visible_end: Pixels,
    /// The shape of the strip this one takes the place of, until the next frame starts from it.
    arriving: Option<StripShape>,
    /// This strip moving from the shape of the strip it took the place of.
    arrival: Option<Arrival<Id>>,
    /// The gaps held open for an item arriving from outside the strip, the last one where it
    /// would land and any others closing.
    incoming: Vec<Incoming<Id>>,
    /// When the incoming gaps last moved toward their lengths.
    incoming_at: Option<Instant>,
}

/// A gap where an item arriving from outside the strip would land, inside the slot of the item
/// it stands before, or of the last item. It is part of the strip's layout, so items that narrow
/// together as the strip fills make room for it as they will for the item once it lands.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Incoming<Id> {
    /// The item the gap stands before, or `None` after the last item.
    before: Option<Id>,
    /// How long the gap is drawn, as it opens or closes.
    extent: Pixels,
    /// How long the gap opens to, or nothing as it closes.
    target: Pixels,
}

/// Where a strip's items and selection stood in the window along its axis, so a strip that takes
/// its place can start from there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripShape {
    /// Where each item started, and its size, in presentation order.
    slots: Vec<(Pixels, Pixels)>,
    /// Where the selection was drawn, from the strip's visible start.
    selection: Option<Span>,
    /// Where the items ended, at most where the strip's visible part ends.
    end: Pixels,
    /// How far the strip was scrolled along its axis.
    scrolled: Pixels,
}

/// A strip moving from the shape of the strip it took the place of to its own. Each frame aims
/// the motion from the latest layout, so a strip last laid out at another size still starts
/// exactly from the other strip's shape.
struct Arrival<Id> {
    from: StripShape,
    /// The last item, whose trailing gap closes the place of the other strip's extra items.
    carrier: Option<Id>,
    /// Whether the last layout predates the arrival, so it shows every item settled.
    first_frame: bool,
}

/// The selection moving to the row `row` of the item `id`.
struct Flight<Id, Row> {
    id: Id,
    row: Row,
    /// Whether the selection glides there from where it was drawn. Otherwise it fades in on the
    /// row, because the item it left was removed.
    glides: bool,
    /// How far the fade has come, or how long the glide may last.
    ease: Ease,
}

/// The selection a removed item held, fading where it was last drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DepartingSelection {
    span: Span,
    /// When the item left.
    left_at: Instant,
    /// How opaque the selection was drawn as the item left, which a selection still fading in
    /// had not reached in full.
    from: f32,
}

impl DepartingSelection {
    /// How opaque the selection still is at `now`.
    fn opacity(&self, now: Instant) -> f32 {
        let faded =
            now.saturating_duration_since(self.left_at).as_secs_f32() / LEAVING_FADE.as_secs_f32();
        self.from * (1.0 - faded.min(1.0))
    }
}

/// Where a [`SelectionOverlay`] drew the selection, and when.
#[derive(Clone, Copy, Debug, PartialEq)]
struct DrawnSelection {
    span: Span,
    at: Instant,
    /// Whether the selection stood on its row.
    settled: bool,
}

/// Where something stands along a strip, from the strip's visible start, and how long it is.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    start: Pixels,
    extent: Pixels,
}

impl Span {
    /// The span `progress` of the way from this one to `to`.
    fn toward(self, to: Self, progress: f32) -> Self {
        Self {
            start: self.start + (to.start - self.start) * progress,
            extent: self.extent + (to.extent - self.extent) * progress,
        }
    }

    /// Whether this span stands within half a point of `other`.
    fn reaches(self, other: Self) -> bool {
        (self.start - other.start).abs() < px(0.5) && (self.extent - other.extent).abs() < px(0.5)
    }

    /// The part of this span inside a visible part `length` long.
    fn inside(self, length: Pixels) -> Self {
        let start = self.start.max(px(0.0));
        let end = (self.start + self.extent).min(length);
        Self {
            start,
            extent: (end - start).max(px(0.0)),
        }
    }

    /// The bounds `place` covers across the strip, and this span covers along it from the strip's
    /// visible start at `visible_start`.
    fn bounds(self, axis: Axis, visible_start: Pixels, place: Bounds<Pixels>) -> Bounds<Pixels> {
        Bounds::new(
            place
                .origin
                .apply_along(axis, |_| visible_start + self.start),
            place.size.apply_along(axis, |_| self.extent),
        )
    }
}

/// How a strip draws its selection this frame. A [`SelectionOverlay`] applies it to the selected
/// row as that frame lays it out.
#[derive(Clone)]
pub(crate) struct SelectionMotion {
    /// How long the selection takes to glide toward the selected row from where it was last
    /// drawn, were the row to hold still, or `None` while it stands on the row.
    glide: Option<Duration>,
    /// How opaque the selection is, as it fades in on a row whose item took it from a removed one.
    opacity: f32,
    /// Whether the selection moves or its row grows in, while which the selection draws whole
    /// inside the strip's visible part rather than cut off at its ends.
    moving: bool,
    /// When the strip prepared the frame, which the selection moves by, as the items do.
    at: Instant,
    /// The selection a removed item held, where it was last drawn, and how opaque it still is as
    /// it fades there.
    departing: Vec<(Span, f32)>,
    /// Where the overlay drew the selection last, and records where it draws it.
    drawn: Rc<Cell<Option<DrawnSelection>>>,
}

/// Which selection a [`SelectionOverlay`] asks its owner to draw a chip for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SelectionPart {
    /// The selection on the selected row.
    Selected,
    /// The selection a removed item held, fading where it was last drawn.
    Departing,
}

/// Where the selected row's face stands in the window this frame, from the layout and scroll
/// that frame settled, or `None` while the strip draws no selection.
type PlaceSelection = dyn FnOnce(&App) -> Option<Bounds<Pixels>>;

/// Builds a selection chip for a face of the given size at the given opacity.
type SelectionFace = dyn Fn(Size<Pixels>, f32, SelectionPart) -> AnyElement;

/// Draws a strip's selection beneath the strip's items, on the selected row as the current frame
/// lays it out and scrolls it, clipped to the strip's visible part. Nothing an item clips, such
/// as the slot of an item growing in, cuts the selection off, and while it moves it keeps whole
/// inside the strip's visible part. The selection a removed item held fades where it was drawn.
///
/// Its parent holds it before the strip's scroll container, so it paints first, and prepaints it
/// after that container with [`prepaint_selection_last`], so it reads the frame's settled layout.
pub(crate) struct SelectionOverlay {
    axis: Axis,
    viewport: ScrollHandle,
    motion: SelectionMotion,
    place: Option<Box<PlaceSelection>>,
    face: Box<SelectionFace>,
}

impl SelectionOverlay {
    /// The selection of the strip `viewport` scrolls along `axis`, moving as `motion` says. `place`
    /// finds the selected row's face in the window once the frame is laid out, and `face` draws
    /// a chip that fills a face.
    pub(crate) fn new(
        axis: Axis,
        viewport: &ScrollHandle,
        motion: SelectionMotion,
        place: impl FnOnce(&App) -> Option<Bounds<Pixels>> + 'static,
        face: impl Fn(Size<Pixels>, f32, SelectionPart) -> AnyElement + 'static,
    ) -> Self {
        Self {
            axis,
            viewport: viewport.clone(),
            motion,
            place: Some(Box::new(place)),
            face: Box::new(face),
        }
    }
}

/// Prepaints the children of a parent holding a [`SelectionOverlay`] first and its strip's scroll
/// container second, with `count` children in all, so the overlay prepaints last.
pub(crate) fn prepaint_selection_last(count: usize) -> smallvec::SmallVec<[usize; 8]> {
    (1..count).chain([0]).collect()
}

impl IntoElement for SelectionOverlay {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for SelectionOverlay {
    type RequestLayoutState = ();
    type PrepaintState = (Vec<AnyElement>, ContentMask<Pixels>);

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
    ) -> (LayoutId, ()) {
        let style = Style {
            position: gpui::Position::Absolute,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let axis = self.axis;
        let viewport = self.viewport.bounds();
        let visible_start = viewport.origin.along(axis);
        let visible_length = viewport.size.along(axis);
        let target = self.place.take().and_then(|place| place(cx));
        let mut chips = Vec::new();
        for (span, opacity) in self.motion.departing.iter().copied() {
            chips.push((
                span.bounds(axis, visible_start, target.unwrap_or(viewport)),
                opacity,
                SelectionPart::Departing,
            ));
        }
        match target {
            None => self.motion.drawn.set(None),
            Some(target) => {
                let keep_whole = |span: Span| {
                    if self.motion.moving {
                        span.inside(visible_length)
                    } else {
                        span
                    }
                };
                let to = Span {
                    start: target.origin.along(axis) - visible_start,
                    extent: target.size.along(axis),
                };
                // A gliding selection closes the same share of its distance to the row in each
                // moment, the share the decay curve closes, so it follows a row that moves, grows
                // or scrolls without a jump and lands on it. It moves by the time the frame's
                // layout was prepared at, as the items around it do.
                let at = self.motion.at;
                let drawn = self
                    .motion
                    .drawn
                    .get()
                    .zip(self.motion.glide)
                    .map(|(drawn, glide)| {
                        let elapsed = at.saturating_duration_since(drawn.at).as_secs_f32()
                            / glide.as_secs_f32();
                        keep_whole(
                            drawn
                                .span
                                .toward(to, 1.0 - (-TIME_CONSTANTS * elapsed).exp()),
                        )
                    })
                    .filter(|drawn| !drawn.reaches(to));
                let settled = drawn.is_none();
                let drawn = drawn.unwrap_or_else(|| keep_whole(to));
                self.motion.drawn.set(Some(DrawnSelection {
                    span: drawn,
                    at,
                    settled,
                }));
                chips.push((
                    drawn.bounds(axis, visible_start, target),
                    self.motion.opacity,
                    SelectionPart::Selected,
                ));
            }
        }
        let mask = ContentMask { bounds: viewport };
        let chips = chips
            .into_iter()
            .map(|(bounds, opacity, chip)| {
                let mut face = (self.face)(bounds.size, opacity, chip);
                window.with_content_mask(Some(mask), |window| {
                    face.prepaint_as_root(
                        bounds.origin,
                        bounds.size.map(AvailableSpace::Definite),
                        window,
                        cx,
                    );
                });
                face
            })
            .collect();
        (chips, mask)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        (chips, mask): &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(*mask), |window| {
            for chip in chips {
                chip.paint(window, cx);
            }
        });
    }
}

/// The item a strip's drag carries, its session, and where a release would land it.
struct Lift<Id> {
    id: Id,
    session: DragSession,
    /// Where the press held the item, from the item's origin.
    grab: Point<Pixels>,
    /// How long the lifted copy is along the strip when it shows only the start of the item's
    /// face, as a Workspace row lifted from its group does.
    length: Option<Pixels>,
    /// Where the lifted copy's origin stands in the window.
    origin: Option<Point<Pixels>>,
    /// Where the pointer last stood in the window.
    pointer: Option<Point<Pixels>>,
    /// When the strip last scrolled under the pointer, while the pointer stays near an end.
    scrolled_at: Option<Instant>,
    landing: Option<Landing>,
    /// Whether the gap holds still while the lifted item sits over an item it can join.
    gap_held: bool,
    /// Each item's slot size along the strip when the gap last opened.
    extents: Vec<Pixels>,
}

/// The position a released item would take among the `len` items of its strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Landing {
    position: usize,
    len: usize,
}

/// How one strip item presents this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct ItemMotion {
    /// How far the item is drawn from its laid-out place.
    pub(crate) offset: Point<Pixels>,
    /// Whether the item is lifted, so its face stays faded where a release would land it.
    pub(crate) lifted: bool,
    /// How much of its lift a released item still shows, from 1 at release to 0 at rest.
    pub(crate) lift: f32,
    /// How far a new item has grown into the strip, from 0 to 1, or `None` once it rests.
    pub(crate) entering: Option<f32>,
    /// The item's slot size along the strip while it grows in, or `None` at its natural size.
    pub(crate) extent: Option<Pixels>,
    /// Whether the item grows from its slot's far end, where the strip holds it in view.
    pub(crate) grows_from_end: bool,
    /// The closing gap before the item's slot, along the strip.
    pub(crate) leading_gap: Pixels,
    /// The closing gap after the item's slot, along the strip.
    pub(crate) trailing_gap: Pixels,
}

impl ItemMotion {
    /// How opaque the item is: faded where a release would land it while lifted, and nearly
    /// opaque while it settles.
    pub(crate) fn opacity(self) -> f32 {
        if self.lifted {
            PLACEHOLDER_OPACITY
        } else {
            lifted_opacity(self.lift)
        }
    }

    /// How far the item has grown into the strip, from 0 to 1.
    pub(crate) fn presence(self) -> f32 {
        self.entering.unwrap_or(1.0)
    }

    /// How opaque a part of the item is that shows whole once the item has grown `whole` of the
    /// way in: hidden until then, so its slot never cuts through it, and fading in after.
    pub(crate) fn reveal(self, whole: f32) -> f32 {
        let presence = self.presence();
        let opaque = (whole + REVEAL_SPAN).min(1.0);
        if presence >= opaque {
            1.0
        } else {
            ((presence - whole) / (opaque - whole)).clamp(0.0, 1.0)
        }
    }

    /// Sizes `slot`, the element that holds the item's place in a strip along `axis`, while the
    /// item grows in, and opens the closing gaps beside it. The item keeps its natural size and
    /// the slot uncovers it as the items making room for it move aside. Nothing of it draws
    /// outside its slot, so its label never crosses a neighbor's. `measure` receives the item's
    /// natural size while it grows, which needs `slot`'s only child to be the item. Both gaps
    /// stay inside the slot, so a scrolling strip counts them and closes them without jumping.
    pub(crate) fn frame_slot(
        self,
        axis: Axis,
        slot: Div,
        measure: impl Fn(Size<Pixels>, &mut App) + 'static,
    ) -> Div {
        let slot = match axis {
            Axis::Horizontal => slot.pl(self.leading_gap).pr(self.trailing_gap),
            Axis::Vertical => slot.pt(self.leading_gap).pb(self.trailing_gap),
        };
        let Some(extent) = self.extent else {
            return slot;
        };
        let gaps = self.leading_gap + self.trailing_gap;
        let slot = match axis {
            Axis::Horizontal => slot.w(extent + gaps),
            Axis::Vertical => slot.h(extent + gaps),
        };
        let slot = slot.overflow_hidden();
        // An item the strip holds at its visible end stands where it settles, uncovered as the
        // items before it make room, instead of past that end.
        let slot = match (self.grows_from_end, axis) {
            (false, _) => slot,
            (true, Axis::Horizontal) => slot.flex().flex_row().justify_end(),
            (true, Axis::Vertical) => slot.flex().flex_col().justify_end(),
        };
        slot.on_children_prepainted(move |bounds, _, cx| {
            if let Some(item) = bounds.first() {
                measure(item.size, cx);
            }
        })
    }

    /// Holds `item` in a slot as [`Self::frame_slot`] does, and draws `before` and `after` in
    /// the closing gaps beside it: the items that left from there, at their own size, clipped as
    /// their gaps close.
    pub(crate) fn frame_item(
        self,
        axis: Axis,
        slot: Div,
        item: AnyElement,
        before: Option<AnyElement>,
        after: Option<AnyElement>,
        measure: impl Fn(Size<Pixels>, &mut App) + 'static,
    ) -> Div {
        if before.is_none() && after.is_none() {
            return self.frame_slot(axis, slot, measure).child(item);
        }
        let item_position = usize::from(before.is_some());
        let gap = |extent: Pixels, content: AnyElement| {
            let gap = div().flex_shrink_0().overflow_hidden();
            match axis {
                Axis::Horizontal => gap.w(extent).h_full(),
                Axis::Vertical => gap.h(extent).w_full(),
            }
            .child(content)
        };
        let (leading_gap, trailing_gap) = (self.leading_gap, self.trailing_gap);
        let before = before.map(|content| gap(leading_gap, content));
        let after = after.map(|content| gap(trailing_gap, content));
        // The gaps that draw nothing stay padding, as in a slot that draws none.
        let unpadded = Self {
            leading_gap: if before.is_some() {
                px(0.0)
            } else {
                leading_gap
            },
            trailing_gap: if after.is_some() {
                px(0.0)
            } else {
                trailing_gap
            },
            extent: self.extent.map(|extent| {
                extent
                    + if before.is_some() {
                        leading_gap
                    } else {
                        px(0.0)
                    }
                    + if after.is_some() {
                        trailing_gap
                    } else {
                        px(0.0)
                    }
            }),
            ..self
        };
        let slot = match axis {
            Axis::Horizontal => slot.flex().flex_row(),
            Axis::Vertical => slot.flex().flex_col(),
        };
        let growing = self.extent.is_some();
        unpadded
            .frame_slot(axis, slot, |_, _| {})
            .when(growing, |slot| {
                slot.on_children_prepainted(move |bounds, _, cx| {
                    if let Some(item) = bounds.get(item_position) {
                        measure(item.size, cx);
                    }
                })
            })
            .children(before)
            .child(item)
            .children(after)
    }
}

impl<Id: Copy + Eq + Hash> ReorderableStrip<Id> {
    /// Moves the selection to the item `selected` from wherever it was drawn, when it differs
    /// from the last refresh. Call it on every render, after [`Self::refresh`].
    pub(crate) fn follow_selection(&mut self, selected: Option<Id>, window: &mut Window, cx: &App) {
        self.follow_row_selection(selected.map(|id| (id, ())), window, cx);
    }
}

impl<Id: Copy + Eq + Hash, Row: Copy + Eq> ReorderableStrip<Id, Row> {
    pub(crate) fn new(axis: Axis) -> Self {
        Self {
            axis,
            joinable: false,
            lift: None,
            slides: HashMap::new(),
            settling: None,
            known: None,
            entering: HashMap::new(),
            grown: Vec::new(),
            growing_from_end: HashSet::new(),
            leading_gaps: HashMap::new(),
            trailing_gaps: HashMap::new(),
            refreshed_at: None,
            laid_out_at: None,
            laid_out: HashMap::new(),
            selected: None,
            drawn_selection: Rc::new(Cell::new(None)),
            flight: None,
            departing_selection: Vec::new(),
            scrolled: px(0.0),
            overflowing: false,
            natural_extent: None,
            squeezed: false,
            visible_end: px(0.0),
            arriving: None,
            arrival: None,
            incoming: Vec::new(),
            incoming_at: None,
        }
    }

    /// Where this strip's items and selection stand in the window, from the strip's `items` as
    /// last painted, or `None` while an item is lifted or the items do not match the strip.
    pub(crate) fn shape(&self, items: &ScrollHandle) -> Option<StripShape> {
        if self.lift.is_some() {
            return None;
        }
        let ids = self.known.as_ref()?;
        let axis = self.axis;
        let painted = painted_item_bounds(items);
        if painted.len() != ids.len() {
            return None;
        }
        let slots = ids
            .iter()
            .zip(&painted)
            .map(|(id, slot)| {
                let item = self.trim(*id, *slot);
                (item.origin.along(axis), item.size.along(axis))
            })
            .collect::<Vec<_>>();
        let selection = self
            .selected
            .and(self.drawn_selection.get())
            .map(|drawn| drawn.span);
        let last = painted.last()?;
        let viewport = items.bounds();
        let end = (last.origin.along(axis) + last.size.along(axis))
            .min(viewport.origin.along(axis) + viewport.size.along(axis));
        Some(StripShape {
            slots,
            selection,
            end,
            scrolled: items.offset().along(axis),
        })
    }

    /// The bounds of the faces the strip's `items` painted each of its `ids` at in the window.
    fn item_bounds(&self, items: &ScrollHandle, ids: &[Id]) -> Vec<Bounds<Pixels>> {
        ids.iter()
            .zip(painted_item_bounds(items))
            .map(|(id, slot)| self.face(*id, slot, items))
            .collect()
    }

    /// Where the strip's `items` showed each of its `ids` in the last frame, in the window: its
    /// face without the gaps its slot holds, moved by any slide, as far as the strip's visible
    /// part shows it. An item the strip shows none of is `None`.
    pub(crate) fn shown_faces(
        &self,
        items: &ScrollHandle,
        ids: &[Id],
        cx: &App,
    ) -> Vec<Option<Bounds<Pixels>>> {
        let viewport = items.bounds();
        let offset = items.offset();
        ids.iter()
            .enumerate()
            .map(|(position, id)| {
                let slot = items.bounds_for_item(position)?;
                let face = self.trim(*id, slot);
                let face = Bounds::new(
                    face.origin + offset + self.motion(*id, cx).offset,
                    face.size,
                );
                let shown = face.intersect(&viewport);
                (shown.size.width > px(0.0) && shown.size.height > px(0.0)).then_some(shown)
            })
            .collect()
    }

    /// The size the strip's `items` painted the item `id` at, the one at `position` among them,
    /// without the closing gaps inside its slot, and whole while it grows in.
    pub(crate) fn item_size(
        &self,
        items: &ScrollHandle,
        id: Id,
        position: usize,
    ) -> Option<Size<Pixels>> {
        items
            .bounds_for_item(position)
            .map(|slot| self.face(id, slot, items).size)
    }

    /// The bounds of the item `id`'s face in `slot`, its slot among the strip's `items` in the
    /// last frame. A new item's face shows whole while its slot grows, from the end of the slot
    /// [`Self::grows_from_end`] names.
    fn face(&self, id: Id, slot: Bounds<Pixels>, items: &ScrollHandle) -> Bounds<Pixels> {
        let item = self.trim(id, slot);
        let axis = self.axis;
        let Some(whole) = self.whole_extent(id, items) else {
            return item;
        };
        let whole = whole.max(item.size.along(axis));
        let lag = if self.grows_from_end(id) {
            item.size.along(axis) - whole
        } else {
            px(0.0)
        };
        Bounds::new(
            item.origin.apply_along(axis, |start| start + lag),
            item.size.apply_along(axis, |_| whole),
        )
    }

    /// How long the whole face of the item `id` is along the strip while it grows in, or `None`
    /// once it rests. Items of one natural size narrow together, so a new one is as long as the
    /// strip's items at rest in `items`; others reach their own natural size.
    fn whole_extent(&self, id: Id, items: &ScrollHandle) -> Option<Pixels> {
        let (_, natural) = self.entering.get(&id)?;
        if self.natural_extent.is_none() {
            return *natural;
        }
        let axis = self.axis;
        self.known
            .iter()
            .flatten()
            .enumerate()
            .filter(|(_, other)| !self.entering.contains_key(other))
            .find_map(|(position, other)| {
                let slot = items.bounds_for_item(position)?;
                Some(self.trim(*other, slot).size.along(axis))
            })
            .or(self.natural_extent)
    }

    /// Whether the item `id` shows its whole face from its slot's far end while it grows in, as
    /// an item does that started growing past the end of a strip with no room left. Its parts,
    /// the selection on it and its lifted copy all stand there.
    fn grows_from_end(&self, id: Id) -> bool {
        self.growing_from_end.contains(&id)
    }

    /// Trims `slot`, the bounds of the item `id`'s slot in the last frame, to the item itself.
    fn trim(&self, id: Id, slot: Bounds<Pixels>) -> Bounds<Pixels> {
        let painted = |gaps: &HashMap<Id, Gap<Id>>| {
            self.refreshed_at
                .zip(gaps.get(&id))
                .map_or(px(0.0), |(at, gap)| gap.at(at))
        };
        let (incoming_lead, incoming_trail) = self.incoming_beside(id);
        let (lead, trail) = (
            painted(&self.leading_gaps) + incoming_lead,
            painted(&self.trailing_gaps) + incoming_trail,
        );
        let axis = self.axis;
        Bounds::new(
            slot.origin.apply_along(axis, |start| start + lead),
            slot.size.apply_along(axis, |size| size - lead - trail),
        )
    }

    /// Starts this strip from `from`, the shape of the strip it takes the place of, on its next
    /// frame. The selection leaves from where the other strip's selection stood, items the other
    /// strip lacked grow in, and the place of its extra items closes after the last item. An
    /// overflowing strip keeps its items in place, since resizing them would move its scroll.
    pub(crate) fn arrive(&mut self, from: StripShape, cx: &App) {
        self.arriving =
            (presence_motion(cx) != spaceterm_ui::ControlMotion::Reduced).then_some(from);
    }

    /// Begins the arrival from the shape [`Self::arrive`] received, once the strip knows its items.
    fn begin_arrival(&mut self, now: Instant, cx: &App) {
        let (Some(from), Some(ids)) = (self.arriving.take(), self.known.clone()) else {
            return;
        };
        if self.lift.is_some() || ids.is_empty() {
            return;
        }
        let motion = presence_motion(cx);
        let reshape = !self.overflowing;
        // The items the other strip lacked grow in from where its end stood, which a gap still
        // closing in it can hold past its last item. Growth left over from an earlier arrival
        // starts again from there.
        let lacked = ids
            .iter()
            .skip(from.slots.len())
            .filter(|_| reshape)
            .map(|id| {
                let natural = match self.entering.get(id) {
                    Some((_, natural)) => *natural,
                    None => self
                        .natural_extent
                        .or_else(|| self.laid_out.get(id).map(|(_, size)| *size)),
                };
                (*id, natural)
            })
            .collect::<Vec<_>>();
        let past_last = from.slots.last().map_or(px(0.0), |(start, size)| {
            (from.end - *start - *size).max(px(0.0))
        });
        let natural = lacked
            .iter()
            .map(|(_, natural)| *natural)
            .sum::<Option<Pixels>>();
        let grown = natural
            .filter(|natural| *natural > px(0.0))
            .map_or(0.0, |natural| (past_last / natural).min(1.0));
        for (id, natural) in lacked {
            self.entering.insert(
                id,
                (
                    Ease::reached(grown, PRESENCE_DURATION, now, motion),
                    natural,
                ),
            );
        }
        let carrier = ids
            .last()
            .copied()
            .filter(|_| reshape && ids.len() < from.slots.len());
        if let Some(carrier) = carrier {
            self.trailing_gaps.insert(
                carrier,
                Gap::closing(px(0.0), Ease::new(PRESENCE_DURATION, now, motion)),
            );
        }
        // The selection leaves from where the other strip drew it, which the overlay finds
        // relative to the strip's visible start, where this strip stands too.
        let flight = self.selected.zip(from.selection).map(|((id, row), from)| {
            self.drawn_selection.set(Some(DrawnSelection {
                span: from,
                at: now,
                settled: false,
            }));
            Flight {
                id,
                row,
                glides: true,
                ease: Ease::new(SELECTION_LIMIT, now, motion),
            }
        });
        // A selection that took its row from a departing item fades in there instead.
        if flight.is_some() {
            self.flight = flight;
        }
        self.arrival = Some(Arrival {
            from,
            carrier,
            first_frame: true,
        });
    }

    /// Aims the arrival's closing gap from the latest layout, and ends the arrival once it no
    /// longer closes.
    fn follow_arrival(&mut self, now: Instant) {
        let Some(arrival) = self.arrival.as_ref() else {
            return;
        };
        let Some(ids) = self.known.as_ref() else {
            self.arrival = None;
            return;
        };
        // Before this strip has a layout, its items stand where the other strip laid out its own,
        // unscrolled, since a strip that never painted has not scrolled.
        let laid_out = !self.laid_out.is_empty();
        let settled = |id: Id| {
            if arrival.first_frame {
                self.laid_out.get(&id).map(|(start, _)| *start)
            } else {
                self.settled_start(id, now)
            }
        };
        let close = arrival
            .carrier
            .filter(|carrier| self.trailing_gaps.contains_key(carrier))
            .and_then(|carrier| {
                let end = if laid_out {
                    let (_, size) = self.laid_out.get(&carrier)?;
                    settled(carrier)? + *size + self.scrolled
                } else {
                    let (start, size) = arrival
                        .from
                        .slots
                        .get(ids.len() - 1)
                        .map(|(start, size)| (*start - arrival.from.scrolled, *size))
                        .unwrap_or((arrival.from.end, px(0.0)));
                    start + size
                };
                Some((carrier, (arrival.from.end - end).max(px(0.0))))
            });
        if let Some((carrier, extent)) = close
            && let Some(place) = self
                .trailing_gaps
                .get_mut(&carrier)
                .and_then(|gap| gap.places.first_mut())
        {
            place.extent = extent;
        }
        if close.is_none() {
            self.arrival = None;
        } else if let Some(arrival) = self.arrival.as_mut() {
            arrival.first_frame = false;
        }
    }

    /// Moves the selection to `selected`, a row of an item, from wherever the selection was
    /// drawn, when the row differs from the last refresh. A selection that leaves a removed item
    /// fades in on its new row instead, while the removed item fades with the selection it held.
    /// Call it on every render, after [`Self::refresh`].
    pub(crate) fn follow_row_selection(
        &mut self,
        selected: Option<(Id, Row)>,
        window: &mut Window,
        cx: &App,
    ) {
        let now = self.refreshed_at.unwrap_or_else(|| now(cx));
        if self.lift.is_some() {
            self.flight = None;
        }
        if selected != self.selected {
            self.arrival = None;
            let previous = std::mem::replace(&mut self.selected, selected);
            let motion = presence_motion(cx);
            let removed = previous.is_some_and(|(previous, _)| {
                self.known
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&previous))
            });
            let drawn = self.drawn_selection.get().filter(|_| previous.is_some());
            // The selection a removed item held fades where it was last drawn, wherever that was
            // on its way, and from as opaque as it was drawn, with the item it fades beside. A
            // selection that only moves leaves every fading one to finish.
            if let Some(drawn) =
                drawn.filter(|_| removed && motion != spaceterm_ui::ControlMotion::Reduced)
            {
                let from = self
                    .flight
                    .as_ref()
                    .filter(|flight| Some((flight.id, flight.row)) == previous && !flight.glides)
                    .map_or(1.0, |flight| flight.ease.progress(now));
                self.departing_selection.push(DepartingSelection {
                    span: drawn.span,
                    left_at: now,
                    from,
                });
            }
            self.flight = selected
                .filter(|_| self.lift.is_none() && motion != spaceterm_ui::ControlMotion::Reduced)
                .and_then(|(id, row)| {
                    if removed {
                        return Some(Flight {
                            id,
                            row,
                            glides: false,
                            ease: Ease::new(LEAVING_FADE, now, motion),
                        });
                    }
                    // The glide starts now from where the selection was last drawn.
                    let drawn = drawn?;
                    self.drawn_selection.set(Some(DrawnSelection {
                        at: now,
                        settled: false,
                        ..drawn
                    }));
                    Some(Flight {
                        id,
                        row,
                        glides: true,
                        ease: Ease::new(SELECTION_LIMIT, now, motion),
                    })
                });
        }
        self.begin_arrival(now, cx);
        self.follow_arrival(now);
        let settled = self
            .drawn_selection
            .get()
            .is_some_and(|drawn| drawn.settled);
        if self
            .flight
            .as_ref()
            .is_some_and(|flight| flight.ease.finished(now) || (flight.glides && settled))
        {
            self.flight = None;
        }
        self.departing_selection
            .retain(|departing| now.saturating_duration_since(departing.left_at) < LEAVING_FADE);
        if self.flight.is_some() || !self.departing_selection.is_empty() {
            window.request_animation_frame();
        }
    }

    /// How the strip draws its selection on the row `row` of the item `id` this frame, for a
    /// [`SelectionOverlay`] to place.
    pub(crate) fn selection_motion(&self, id: Id, row: Row, cx: &App) -> SelectionMotion {
        // The selection moves by the time the frame's items were prepared at, so it keeps pace
        // with their growth however long the frame takes to paint.
        let now = self.refreshed_at.unwrap_or_else(|| now(cx));
        let flight = self
            .flight
            .as_ref()
            .filter(|flight| flight.id == id && flight.row == row && self.lift.is_none());
        // A selection moving to an item that grows in keeps pace with its growth, so it never
        // runs past the end of the item's slot.
        let glide = if self.entering.contains_key(&id) {
            PRESENCE_DURATION
        } else {
            SELECTION_DURATION
        };
        SelectionMotion {
            glide: flight.filter(|flight| flight.glides).map(|_| glide),
            opacity: flight
                .filter(|flight| !flight.glides)
                .map_or(1.0, |flight| flight.ease.progress(now)),
            moving: flight.is_some() || self.entering.contains_key(&id),
            at: now,
            departing: self
                .departing_selection
                .iter()
                .filter(|_| self.lift.is_none())
                .map(|departing| (departing.span, departing.opacity(now)))
                .collect(),
            drawn: Rc::clone(&self.drawn_selection),
        }
    }

    /// Finds, once a frame is laid out, where the face of the item `id`, one of the strip's `ids`
    /// in `items`, stands in the window: its slot without the closing gaps the slot holds, moved
    /// by any slide. An item growing in has its whole face, from the end of its slot it grows
    /// from.
    pub(crate) fn place_face(
        &self,
        id: Id,
        ids: &[Id],
        items: &ScrollHandle,
        cx: &App,
    ) -> impl FnOnce(&App) -> Option<Bounds<Pixels>> + use<Id, Row> {
        let axis = self.axis;
        let position = ids.iter().position(|candidate| *candidate == id);
        let motion = self.motion(id, cx);
        let from_end = self.grows_from_end(id);
        // Items of one natural size narrow together, so a new one is as long as those at rest.
        // Others reach their own natural size.
        let uniform = self.natural_extent.is_some();
        let natural = self
            .natural_extent
            .or_else(|| self.entering.get(&id).and_then(|(_, natural)| *natural));
        let slots = ids
            .iter()
            .map(|id| {
                let motion = self.motion(*id, cx);
                (
                    motion.leading_gap,
                    motion.trailing_gap,
                    motion.entering.is_none(),
                )
            })
            .collect::<Vec<_>>();
        let items = items.clone();
        move |_| {
            let position = position?;
            let offset = items.offset();
            let face = |position: usize| {
                let slot = items.bounds_for_item(position)?;
                let (lead, trail, _) = slots.get(position)?;
                Some((
                    slot.origin.along(axis) + offset.along(axis) + *lead,
                    slot.size.along(axis) - *lead - *trail,
                    slot,
                ))
            };
            let (start, extent, slot) = face(position)?;
            let (start, extent) = if motion.entering.is_some() {
                let resting = slots
                    .iter()
                    .enumerate()
                    .filter(|(_, (_, _, rests))| uniform && *rests)
                    .find_map(|(near, _)| face(near).map(|(_, extent, _)| extent));
                let whole = resting.or(natural).unwrap_or(extent).max(extent);
                if from_end {
                    (start + extent - whole, whole)
                } else {
                    (start, whole)
                }
            } else {
                (start, extent)
            };
            Some(Bounds::new(
                (slot.origin + offset).apply_along(axis, |_| start) + motion.offset,
                slot.size.apply_along(axis, |_| extent),
            ))
        }
    }

    /// Where the item `id` starts along the strip once every item has grown in and every gap has
    /// closed. A new item starts where the item after it, or the end of the item before it, stood
    /// in the last frame, and settles as far beyond as the items before it still grew then. The
    /// last frame's layout pairs with the growth of that frame's time, not `now`'s.
    fn settled_start(&self, id: Id, now: Instant) -> Option<Pixels> {
        let now = self.laid_out_at.unwrap_or(now);
        if let Some((start, _)) = self.laid_out.get(&id) {
            return Some(*start - self.displacement(id, now));
        }
        let ids = self.known.as_ref()?;
        let position = ids.iter().position(|candidate| *candidate == id)?;
        let next = ids[position + 1..]
            .iter()
            .find_map(|next| self.laid_out.get(next))
            .map(|(start, _)| *start);
        let previous = ids[..position]
            .iter()
            .rev()
            .find_map(|previous| self.laid_out.get(previous))
            .map(|(start, size)| *start + *size);
        next.or(previous)
            .map(|start| start - self.displacement(id, now))
    }

    /// How far the item `id` starts along the strip from where it settles, while items before it
    /// grow in and gaps before it close.
    fn displacement(&self, id: Id, now: Instant) -> Pixels {
        let Some(ids) = self.known.as_ref() else {
            return px(0.0);
        };
        let Some(position) = ids.iter().position(|candidate| *candidate == id) else {
            return px(0.0);
        };
        let gap =
            |gaps: &HashMap<Id, Gap<Id>>, id: &Id| gaps.get(id).map_or(px(0.0), |gap| gap.at(now));
        ids[..position]
            .iter()
            .fold(gap(&self.leading_gaps, &id), |shift, before| {
                let growing = self
                    .entering
                    .get(before)
                    .filter(|(ease, _)| !ease.finished(now))
                    .and_then(|(ease, natural)| {
                        natural.map(|natural| natural * (ease.at(now) - 1.0))
                    })
                    .unwrap_or(px(0.0));
                shift + gap(&self.leading_gaps, before) + gap(&self.trailing_gaps, before) + growing
            })
    }

    /// Keeps the new item `id`, one of the strip's `ids`, scrolled into view while it grows and
    /// on the frame it reaches full size, so a strip that revealed it before it had a size still
    /// shows it once it does.
    pub(crate) fn reveal_while_entering(&self, id: Id, items: &ScrollHandle, ids: &[Id]) {
        if !self.entering.contains_key(&id) && !self.grown.contains(&id) {
            return;
        }
        if let Some(position) = ids.iter().position(|candidate| *candidate == id) {
            items.scroll_to_item(position);
        }
    }

    /// Records the natural size of the new item `id`, which its slot holds back while it grows.
    pub(crate) fn measure(&mut self, id: Id, size: Size<Pixels>) {
        let axis = self.axis;
        if let Some((_, natural)) = self.entering.get_mut(&id) {
            *natural = Some(size.along(axis));
        }
    }

    /// Records where each item of the last frame was laid out, when the strip's children match it.
    fn record_layout(&mut self, items: &ScrollHandle) {
        let axis = self.axis;
        let Some(known) = self.known.as_ref() else {
            return;
        };
        self.scrolled = items.offset().along(axis);
        self.laid_out_at = self.refreshed_at;
        let viewport = items.bounds();
        let visible_end = viewport.origin.along(axis) + viewport.size.along(axis);
        let bounds = (0..known.len())
            .map(|position| items.bounds_for_item(position))
            .collect::<Option<Vec<_>>>()
            .filter(|bounds| bounds.len() == known.len() && items.children_count() == known.len());
        self.laid_out = bounds
            .map(|bounds| {
                known
                    .iter()
                    .zip(bounds)
                    .map(|(id, slot)| {
                        let item = self.trim(*id, slot);
                        (*id, (item.origin.along(axis), item.size.along(axis)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.overflowing = self
            .laid_out
            .values()
            .any(|(start, size)| *start + *size > visible_end + px(0.5));
        // Items that rest narrower than they would with room show a strip that has filled.
        let entering = &self.entering;
        self.squeezed = self.natural_extent.is_some_and(|natural| {
            self.laid_out
                .iter()
                .any(|(id, (_, size))| !entering.contains_key(id) && *size < natural - px(0.5))
        });
        self.visible_end = visible_end - self.scrolled;
    }

    /// Gives every item `natural` along the strip while the strip has room, for a strip whose
    /// owner narrows its items together as it fills. New items grow toward that size, and a new
    /// item at the end of a filled strip grows from that end. Call it before [`Self::refresh`].
    pub(crate) fn set_natural_extent(&mut self, natural: Pixels) {
        self.natural_extent = Some(natural);
    }

    /// The size of the new item at `position` among the strip's `ids`, estimated from a neighbor,
    /// when it reaches past the visible end of an overflowing strip once grown, judged from the
    /// last frame's layout. A strip that fits its items ends at its last item, and grows with a
    /// new one.
    fn reaches_past_visible_end(&self, ids: &[Id], position: usize) -> Option<Pixels> {
        if !self.overflowing && !self.squeezed {
            return None;
        }
        let laid_out = |candidate: &Id| self.laid_out.get(candidate).copied();
        let next = ids[position + 1..].iter().find_map(laid_out);
        let previous = ids[..position].iter().rev().find_map(laid_out);
        let start = next
            .map(|(start, _)| start)
            .or(previous.map(|(start, size)| start + size));
        // A neighbor that is still growing in has its full size ahead of it. A filled strip
        // ends where its visible part does, so any new item past its last one reaches past it.
        let size = |candidate: &Id| {
            if let Some(natural) = self.natural_extent {
                return self.laid_out.contains_key(candidate).then_some(natural);
            }
            let (_, size) = self.laid_out.get(candidate)?;
            let natural = self
                .entering
                .get(candidate)
                .and_then(|(_, natural)| *natural);
            Some(natural.unwrap_or(*size))
        };
        let size = ids[..position]
            .iter()
            .rev()
            .chain(&ids[position + 1..])
            .find_map(size);
        start
            .zip(size)
            .filter(|(start, size)| *start + *size > self.visible_end + px(0.5))
            .map(|(_, size)| size)
    }

    /// Starts new items growing into the strip and closes the gaps removed items leave, when the
    /// strip's `ids` differ from the last refresh. The first refresh only records them.
    fn follow_changes(&mut self, items: &ScrollHandle, ids: &[Id], cx: &App) {
        let Some(known) = self.known.replace(ids.to_vec()) else {
            return;
        };
        if known == ids {
            return;
        }
        self.arrival = None;
        let now = now(cx);
        let motion = presence_motion(cx);
        // An item that returns while its old place still closes draws only where it returns. Its
        // old place stays empty, so what follows it in the gap keeps its position.
        for leaving in self
            .leading_gaps
            .values_mut()
            .chain(self.trailing_gaps.values_mut())
            .flat_map(|gap| gap.leaving.iter_mut())
        {
            if leaving.id.is_some_and(|id| ids.contains(&id)) {
                leaving.id = None;
            }
        }
        // Items that replace removed ones grow as those places close, on the same curve, so the
        // strip keeps its length through the exchange.
        let exchange = known.iter().any(|id| !ids.contains(id));
        for (position, id) in ids.iter().enumerate() {
            if known.contains(id) {
                continue;
            }
            // An item that grows from the strip's end starts its face at the estimated size, so
            // its face lines up before it is measured.
            let estimate = self.reaches_past_visible_end(ids, position);
            if estimate.is_some() {
                self.growing_from_end.insert(*id);
            }
            let ease = if exchange {
                Ease::closing(now, motion)
            } else {
                Ease::new(PRESENCE_DURATION, now, motion)
            };
            self.entering
                .insert(*id, (ease, self.natural_extent.or(estimate)));
        }
        let bounds = (0..known.len())
            .map(|position| items.bounds_for_item(position))
            .collect::<Option<Vec<_>>>()
            .filter(|bounds| bounds.len() == known.len());
        // The bounds are the last frame's, so the gaps they hold, and what shows in them, are
        // measured at that frame's time too.
        let painted_at = self.refreshed_at.unwrap_or(now);
        // How many items this change already put before each carrier's older leading gap.
        let mut placed = HashMap::new();
        for (position, id) in known.iter().enumerate() {
            if ids.contains(id) {
                continue;
            }
            // An item that leaves while it grows in keeps showing only what its growth showed.
            let entering = self
                .entering
                .remove(id)
                .map(|(ease, _)| ease)
                .filter(|ease| !ease.finished(painted_at))
                .map(|ease| ease.at(painted_at));
            let grows_from_end = self.growing_from_end.remove(id);
            // The slot's bounds hold its own closing gaps, and the items that left from them,
            // which keep what they show of themselves as their place joins this one.
            let lead = self.leading_gaps.remove(id);
            let trail = self.trailing_gaps.remove(id);
            let Some(bounds) = bounds.as_ref() else {
                continue;
            };
            let extent = bounds[position].size.along(self.axis);
            let painted =
                |gap: &Option<Gap<Id>>| gap.as_ref().map_or(px(0.0), |gap| gap.at(painted_at));
            let face = (extent - painted(&lead) - painted(&trail)).max(px(0.0));
            // The slot's own gaps keep closing as they were, and its item's place starts
            // closing now.
            let mut places = lead
                .iter()
                .chain(trail.iter())
                .flat_map(|gap| gap.places.iter().copied())
                .chain([Closing {
                    extent: face,
                    ease: Ease::closing(now, motion),
                }])
                .collect::<Vec<_>>();
            let shown = |gap: Option<Gap<Id>>| {
                gap.map_or_else(Vec::new, |gap| {
                    let open = gap.at(painted_at);
                    shown_leaving(gap.leaving, open)
                })
            };
            let leaving = shown(lead)
                .into_iter()
                .chain([Leaving {
                    id: Some(*id),
                    extent: face,
                    left_at: now,
                    entering,
                    grows_from_end,
                }])
                .chain(shown(trail))
                .collect::<Vec<_>>();
            let next = known[position + 1..].iter().find(|id| ids.contains(id));
            let previous = known[..position].iter().rev().find(|id| ids.contains(id));
            let (gap, carrier, leading) = match (next, previous) {
                (Some(next), _) => (self.leading_gaps.remove(next), *next, true),
                (None, Some(previous)) => (self.trailing_gaps.remove(previous), *previous, false),
                (None, None) => continue,
            };
            let open = gap.as_ref().map_or(px(0.0), |gap| gap.at(painted_at));
            let older_places = gap.as_ref().map_or_else(Vec::new, |gap| gap.places.clone());
            let mut older = gap.map_or_else(Vec::new, |gap| shown_leaving(gap.leaving, open));
            // Items this change removes stand in order before an older leading gap, which is
            // nearer its carrier, and after an older trailing gap.
            if leading {
                let at = placed.entry(carrier).or_insert(0);
                *at = (*at).min(older.len());
                let count = leaving.len();
                older.splice(*at..*at, leaving);
                *at += count;
            } else {
                older.extend(leaving);
            }
            let gaps = if leading {
                &mut self.leading_gaps
            } else {
                &mut self.trailing_gaps
            };
            // The carrier's gap keeps closing as it was, and gains these places.
            places.splice(0..0, older_places);
            gaps.insert(
                carrier,
                Gap {
                    places,
                    leaving: older,
                },
            );
        }
    }

    /// When the strip prepared its last frame.
    pub(crate) fn painted_at(&self, cx: &App) -> Instant {
        self.refreshed_at.unwrap_or_else(|| now(cx))
    }

    /// The removed items whose place still closes before the item `id`, and after it, in
    /// presentation order.
    pub(crate) fn leaving_beside(&self, id: Id) -> (&[Leaving<Id>], &[Leaving<Id>]) {
        fn leaving<'a, Id: Eq + Hash>(
            gaps: &'a HashMap<Id, Gap<Id>>,
            id: &Id,
        ) -> &'a [Leaving<Id>] {
            gaps.get(id).map_or(&[], |gap| gap.leaving.as_slice())
        }
        (
            leaving(&self.leading_gaps, &id),
            leaving(&self.trailing_gaps, &id),
        )
    }

    /// Every removed item whose place still closes.
    pub(crate) fn leaving(&self) -> impl Iterator<Item = Id> + '_ {
        self.leading_gaps
            .values()
            .chain(self.trailing_gaps.values())
            .flat_map(|gap| gap.leaving.iter().filter_map(|leaving| leaving.id))
    }

    /// Lifts the item `id`, one of the strip's `ids` in presentation order. It keeps its place
    /// until a release lands it elsewhere. A lifted copy that shows only the start of the item's
    /// face is `length` long along the strip; otherwise it shows the whole face.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn begin(
        &mut self,
        id: Id,
        length: Option<Pixels>,
        session: DragSession,
        items: &ScrollHandle,
        ids: &[Id],
        window: &Window,
        cx: &App,
    ) {
        // An item grabbed while it still settles is drawn short of its place, so the press holds
        // it from where it is drawn.
        let drawn = ids
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|position| self.item_bounds(items, ids).get(position).copied())
            .map(|face| {
                face.origin
                    + self
                        .slides
                        .get(&id)
                        .map_or_else(Point::default, |slide| slide.at(now(cx)))
            });
        self.slides.clear();
        self.settling = None;
        self.arriving = None;
        self.arrival = None;
        let grab = drawn.map_or_else(Point::default, |drawn| grab_point(window, cx) - drawn);
        self.lift = Some(Lift {
            id,
            session,
            grab,
            length,
            origin: None,
            pointer: None,
            scrolled_at: None,
            landing: None,
            gap_held: false,
            extents: Vec::new(),
        });
    }

    /// Prepares a frame of the strip's `ids`, in presentation order. Ends a lift whose drag GPUI
    /// no longer carries, so a drag released while the strip was hidden leaves nothing behind.
    /// Scrolls the strip while the pointer holds near either end, and keeps frames coming while
    /// it scrolls or items slide. Call it on every render.
    pub(crate) fn refresh(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        window: &mut Window,
        cx: &App,
    ) {
        if self
            .lift
            .as_ref()
            .is_some_and(|lift| !lift.session.is_active(cx))
        {
            self.lift = None;
            self.slides.clear();
        }
        let now = now(cx);
        self.finish_reduced_motion(now, cx);
        self.record_layout(items);
        self.follow_changes(items, ids, cx);
        self.follow_incoming(now, window);
        self.autoscroll(items, ids, window, cx);
        self.slides
            .retain(|_, slide| !slide.finished(now) || slide.to != Point::default());
        self.grown.clear();
        let grown = &mut self.grown;
        self.entering.retain(|id, (ease, _)| {
            let finished = ease.finished(now);
            if finished {
                grown.push(*id);
            }
            !finished
        });
        let entering = &self.entering;
        self.growing_from_end.retain(|id| entering.contains_key(id));
        for gap in self
            .leading_gaps
            .values_mut()
            .chain(self.trailing_gaps.values_mut())
        {
            gap.places.retain(|place| !place.ease.finished(now));
        }
        self.leading_gaps.retain(|_, gap| !gap.closed(now));
        self.trailing_gaps.retain(|_, gap| !gap.closed(now));
        self.refreshed_at = Some(now);
        if !self.entering.is_empty()
            || !self.leading_gaps.is_empty()
            || !self.trailing_gaps.is_empty()
        {
            window.request_animation_frame();
        }
        if self
            .settling
            .is_some_and(|id| !self.slides.contains_key(&id))
        {
            self.settling = None;
        }
        if self.slides.values().any(|slide| !slide.finished(now)) {
            window.request_animation_frame();
        }
    }

    /// Ends the motion that started before motion was reduced, so the strip shows its items at
    /// rest. A drag's displacement still follows the pointer.
    fn finish_reduced_motion(&mut self, now: Instant, cx: &App) {
        let reduced = spaceterm_ui::ControlMotion::Reduced;
        if presence_motion(cx) == reduced {
            for (ease, _) in self.entering.values_mut() {
                *ease = Ease::new(PRESENCE_DURATION, now, reduced);
            }
            self.leading_gaps.clear();
            self.trailing_gaps.clear();
            self.flight = None;
            self.departing_selection.clear();
            self.arriving = None;
            self.arrival = None;
        }
        if super::appearance_runtime::control_motion(cx) == reduced {
            for slide in self.slides.values_mut() {
                *slide = Slide::new(slide.to, slide.to, SETTLE_DURATION, now, reduced);
            }
        }
    }

    /// Takes `ids` in place of the strip's items, as the strip of another Workspace takes this
    /// one's place: the selection leaves from where the old items drew it, the items the old ones
    /// lacked grow in, and the place of their extra items closes. Call it before
    /// [`Self::refresh`] on the frame that first shows `ids`. A strip with a lifted item keeps
    /// its items until the drag ends.
    ///
    /// The items among `departed` left the strip as it gave way, as a closed Tab can take its
    /// Worktree's strip with it. They fade where they stood before their places close, and the
    /// selection one of them held fades in on its new row where that row stands.
    pub(crate) fn take_place(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        departed: &[Id],
        cx: &App,
    ) {
        if self.lift.is_some() {
            return;
        }
        let mut from = self.arriving.take().or_else(|| self.shape(items));
        let painted_at = self.refreshed_at.unwrap_or_else(|| now(cx));
        let known = self.known.clone().unwrap_or_default();
        // Each departing item, where it stood among the old items, and what of it showed.
        let departing = from.as_ref().map_or_else(Vec::new, |from| {
            known
                .iter()
                .zip(&from.slots)
                .enumerate()
                .filter(|(_, (id, _))| departed.contains(id))
                .map(|(position, (id, (_, extent)))| {
                    let entering = self
                        .entering
                        .get(id)
                        .filter(|(ease, _)| !ease.finished(painted_at))
                        .map(|(ease, _)| ease.at(painted_at));
                    (
                        position,
                        Leaving {
                            id: Some(*id),
                            extent: *extent,
                            left_at: now(cx),
                            entering,
                            grows_from_end: self.growing_from_end.contains(id),
                        },
                    )
                })
                .collect::<Vec<_>>()
        });
        let selected = self
            .selected
            .filter(|(id, _)| departing.iter().any(|(_, leaving)| leaving.id == Some(*id)));
        let drawn = self.drawn_selection.get();
        if let Some(from) = from.as_mut()
            && !departing.is_empty()
        {
            // The other items arrive as from a strip without the departing ones.
            from.slots = from
                .slots
                .iter()
                .enumerate()
                .filter(|(position, _)| {
                    !departing.iter().any(|(departing, _)| departing == position)
                })
                .map(|(_, slot)| *slot)
                .collect();
            if selected.is_some() {
                from.selection = None;
            }
        }
        *self = Self {
            known: Some(ids.to_vec()),
            joinable: self.joinable,
            ..Self::new(self.axis)
        };
        if let Some(from) = from {
            self.arrive(from, cx);
        }
        let motion = presence_motion(cx);
        if motion == spaceterm_ui::ControlMotion::Reduced {
            return;
        }
        let now = now(cx);
        for (position, leaving) in departing {
            let (carrier, leading) = match ids.get(position).or(ids.last()) {
                Some(carrier) if position < ids.len() => (*carrier, true),
                Some(carrier) => (*carrier, false),
                None => continue,
            };
            let gaps = if leading {
                &mut self.leading_gaps
            } else {
                &mut self.trailing_gaps
            };
            let gap = gaps.entry(carrier).or_default();
            gap.places.push(Closing {
                extent: leaving.extent,
                ease: Ease::closing(now, motion),
            });
            gap.leaving.push(leaving);
        }
        // The selection that left with a departing item fades where it was drawn, and in on the
        // row that takes it.
        if selected.is_some() {
            self.selected = selected;
            self.drawn_selection.set(drawn);
        }
    }

    /// Whether the item `id` is growing into the strip, or reached its full size this frame.
    pub(crate) fn is_entering(&self, id: Id) -> bool {
        self.entering.contains_key(&id) || self.grown.contains(&id)
    }

    pub(crate) fn dragged(&self) -> Option<Id> {
        self.lift.as_ref().map(|lift| lift.id)
    }

    /// Where the lifted copy's origin stands in the window.
    pub(crate) fn lifted_origin(&self) -> Option<Point<Pixels>> {
        self.lift.as_ref().and_then(|lift| lift.origin)
    }

    /// How the item `id` presents this frame.
    pub(crate) fn motion(&self, id: Id, cx: &App) -> ItemMotion {
        self.motion_at(id, now(cx))
    }

    /// How the item `id` presented in the last frame the strip prepared, which the strip's
    /// last layout shows.
    pub(crate) fn painted_motion(&self, id: Id, cx: &App) -> ItemMotion {
        self.motion_at(id, self.painted_at(cx))
    }

    fn motion_at(&self, id: Id, now: Instant) -> ItemMotion {
        let slide = self.slides.get(&id);
        let entering = self
            .entering
            .get(&id)
            .filter(|(ease, _)| !ease.finished(now));
        let grows_from_end = entering.is_some() && self.growing_from_end.contains(&id);
        ItemMotion {
            offset: slide.map_or_else(Point::default, |slide| slide.at(now)),
            lifted: self.dragged() == Some(id),
            lift: slide
                .filter(|_| self.settling == Some(id))
                .map_or(0.0, |slide| 1.0 - decay(slide.progress(now))),
            entering: entering.map(|(ease, _)| ease.at(now)),
            extent: entering
                .map(|(ease, natural)| natural.map_or(px(0.0), |natural| natural * ease.at(now))),
            grows_from_end,
            leading_gap: self
                .leading_gaps
                .get(&id)
                .map_or(px(0.0), |gap| gap.at(now))
                + self.incoming_beside(id).0,
            trailing_gap: self
                .trailing_gaps
                .get(&id)
                .map_or(px(0.0), |gap| gap.at(now))
                + self.incoming_beside(id).1,
        }
    }

    /// How long the incoming gaps before the item `id` and after it are drawn.
    fn incoming_beside(&self, id: Id) -> (Pixels, Pixels) {
        let last = self.known.as_ref().and_then(|ids| ids.last()) == Some(&id);
        self.incoming
            .iter()
            .fold((px(0.0), px(0.0)), |(lead, trail), gap| match gap.before {
                Some(before) if before == id => (lead + gap.extent, trail),
                None if last => (lead, trail + gap.extent),
                _ => (lead, trail),
            })
    }

    /// Follows the pointer with the lifted copy, held inside the strip, and the slot a release would
    /// land the item in. Returns whether either changed.
    pub(crate) fn track(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
        cx: &App,
    ) -> bool {
        self.follow(items, ids, pointer, false, cx)
    }

    /// Moves the lifted copy to `pointer` like [`Self::track`], while the gap stays where it is,
    /// as over an item the lifted one can join.
    pub(crate) fn track_holding_gap(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
        cx: &App,
    ) -> bool {
        self.follow(items, ids, pointer, true, cx)
    }

    fn follow(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
        hold_gap: bool,
        cx: &App,
    ) -> bool {
        let axis = self.axis;
        // The lifted copy follows the item's face, and the others move by whole slots, which
        // carry their closing gaps with them.
        let faces = self.item_bounds(items, ids);
        let slots = painted_item_bounds(items);
        let Some(lift) = self.lift.as_mut() else {
            return false;
        };
        let current = ids
            .iter()
            .position(|candidate| *candidate == lift.id)
            .filter(|_| faces.len() == ids.len() && slots.len() == ids.len());
        let copy = current.map(|current| {
            let face = faces[current];
            lift.length.map_or(face, |length| {
                Bounds::new(face.origin, face.size.apply_along(axis, |_| length))
            })
        });
        let origin = copy.map_or(pointer - lift.grab, |copy| {
            confine(axis, items.bounds(), copy, pointer - lift.grab)
        });
        // A hold keeps the gap the pointer opened as it reached the item it can join.
        let landing = if hold_gap && lift.gap_held {
            lift.landing
        } else {
            current.zip(copy).map(|(current, copy)| Landing {
                position: landing_position(
                    &spans(axis, &slots),
                    current,
                    if self.joinable {
                        f32::from(pointer.along(axis))
                    } else {
                        f32::from(origin.along(axis) + copy.size.along(axis) / 2.0)
                    },
                ),
                len: ids.len(),
            })
        };
        lift.gap_held = hold_gap;
        lift.pointer = Some(pointer);
        let moved = lift.origin != Some(origin);
        lift.origin = Some(origin);
        let relanded = lift.landing != landing;
        lift.landing = landing;
        // Items that grow or close their gaps mid-drag change how far the others shift.
        let extents = slots
            .iter()
            .map(|slot| slot.size.along(axis))
            .collect::<Vec<_>>();
        let resized = lift.extents != extents;
        lift.extents = extents;
        if relanded || resized {
            self.open_gap(ids, &slots, cx);
        }
        moved || relanded
    }

    /// Where the pointer carrying the lifted item last stood, which autoscroll keeps following.
    pub(crate) fn lifted_pointer(&self) -> Option<Point<Pixels>> {
        self.lift.as_ref().and_then(|lift| lift.pointer)
    }

    /// A strip whose dragged item can join another item. Its pointer decides where the item
    /// lands, and [`Self::covered_item`] names the item it would join.
    pub(crate) fn joinable(axis: Axis) -> Self {
        Self {
            joinable: true,
            ..Self::new(axis)
        }
    }

    /// The item, other than the lifted one, whose drawn face holds `pointer` in its middle
    /// `share`. A strip whose items can join one another holds its gap still there, so the item
    /// stays under the pointer.
    pub(crate) fn covered_item(
        &self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
        share: f32,
    ) -> Option<Id> {
        let axis = self.axis;
        let lift = self.lift.as_ref()?;
        // Only an item the strip shows can be joined; the strip clips the rest.
        if !items.bounds().contains(&pointer) {
            return None;
        }
        let faces = self.item_bounds(items, ids);
        let slots = painted_item_bounds(items);
        let current = ids
            .iter()
            .position(|candidate| *candidate == lift.id)
            .filter(|_| faces.len() == ids.len() && slots.len() == ids.len())?;
        let spans = spans(axis, &slots);
        let along = f32::from(pointer.along(axis));
        // A held gap keeps the joined item under the pointer. Otherwise the items stand where
        // the pointer would land the lifted one, as the next track draws them.
        let landing = if lift.gap_held {
            lift.landing.map_or(current, |landing| landing.position)
        } else {
            landing_position(&spans, current, along)
        };
        covered_position(&spans, current, landing, along, share).map(|position| ids[position])
    }

    /// Where an item arriving from outside the strip lands if released at `pointer`: before the
    /// first of `ids` whose face's middle lies past the pointer, or after the last. The faces
    /// stand where the incoming gap pushed them, so the gap holds still under a still pointer.
    pub(crate) fn incoming_position(
        &self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
    ) -> Option<usize> {
        let slots = painted_item_bounds(items);
        let faces = ids
            .iter()
            .zip(&slots)
            .map(|(id, slot)| self.trim(*id, *slot))
            .collect::<Vec<_>>();
        (slots.len() == ids.len()).then(|| {
            incoming_position(
                &spans(self.axis, &faces),
                f32::from(pointer.along(self.axis)),
            )
        })
    }

    /// Opens a gap before the item at `position` among `ids`, or after the last, for an item
    /// arriving from outside the strip, as a Pane that becomes a Tab, and closes any gap opened
    /// elsewhere. The gap opens to as long as the strip's items at rest when they all narrow
    /// together, and to `extent` otherwise. `None` closes every gap.
    pub(crate) fn hold_incoming(&mut self, ids: &[Id], gap: Option<(usize, Pixels)>, cx: &App) {
        let before = gap.map(|(position, _)| ids.get(position).copied());
        // Items that narrow together make room for an arriving one, so it lands as long as they
        // stand once it has, which the gap's length in the last frame already squeezed them to.
        let resting = self.natural_extent.and_then(|natural| {
            self.laid_out
                .iter()
                .filter(|(id, _)| !self.entering.contains_key(id))
                .map(|(_, (_, size))| *size)
                .next()
                .or(Some(natural))
        });
        let target = gap.map(|(_, extent)| resting.unwrap_or(extent));
        let mut opened = false;
        for incoming in &mut self.incoming {
            incoming.target = match (before, target) {
                (Some(before), Some(target)) if incoming.before == before && !opened => {
                    opened = true;
                    target
                }
                _ => px(0.0),
            };
        }
        if let (Some(before), Some(target), false) = (before, target, opened) {
            self.incoming.push(Incoming {
                before,
                extent: px(0.0),
                target,
            });
        }
        if super::appearance_runtime::control_motion(cx) == spaceterm_ui::ControlMotion::Reduced {
            for incoming in &mut self.incoming {
                incoming.extent = incoming.target;
            }
            self.incoming.retain(|incoming| incoming.target > px(0.0));
        }
    }

    /// Moves each incoming gap toward its length, closing the same share of the distance in each
    /// moment as a slide does, and drops the gaps that closed.
    fn follow_incoming(&mut self, now: Instant, window: &mut Window) {
        let elapsed = self
            .incoming_at
            .replace(now)
            .map_or(0.0, |at| now.saturating_duration_since(at).as_secs_f32());
        let share = 1.0 - (-TIME_CONSTANTS * elapsed / SHIFT_DURATION.as_secs_f32()).exp();
        for incoming in &mut self.incoming {
            incoming.extent += (incoming.target - incoming.extent) * share;
            if (incoming.target - incoming.extent).abs() < px(0.5) {
                incoming.extent = incoming.target;
            }
        }
        self.incoming
            .retain(|incoming| incoming.target > px(0.0) || incoming.extent > px(0.0));
        if self
            .incoming
            .iter()
            .any(|incoming| incoming.extent != incoming.target)
        {
            window.request_animation_frame();
        }
    }

    /// Where the open incoming gap starts along the strip in the strip's `items`, in window
    /// coordinates, and how long it is drawn.
    pub(crate) fn incoming_gap(&self, items: &ScrollHandle) -> Option<(Pixels, Pixels)> {
        let gap = self.incoming.iter().find(|gap| gap.target > px(0.0))?;
        let ids = self.known.as_ref()?;
        let axis = self.axis;
        let slots = painted_item_bounds(items);
        let start = match gap.before {
            Some(before) => {
                let position = ids.iter().position(|id| *id == before)?;
                let face = self.trim(before, *slots.get(position)?);
                face.origin.along(axis) - gap.extent
            }
            None => {
                let last = slots.last()?;
                last.origin.along(axis) + last.size.along(axis) - gap.extent
            }
        };
        Some((start, gap.extent))
    }

    /// How long the open incoming gap opens to, which the arriving item lands at.
    pub(crate) fn incoming_length(&self) -> Option<Pixels> {
        self.incoming
            .iter()
            .find(|gap| gap.target > px(0.0))
            .map(|gap| gap.target)
    }

    /// Lands the arriving item `id`, which its owner just placed at `position` among its items,
    /// where the incoming gap stood. It takes the gap's place in the strip's layout at the length
    /// the gap held, and shows there whole rather than growing in.
    pub(crate) fn land_incoming(&mut self, id: Id, position: usize) {
        self.incoming.clear();
        if let Some(known) = self.known.as_mut()
            && !known.contains(&id)
        {
            known.insert(position.min(known.len()), id);
        }
    }

    /// Scrolls the strip toward the end the pointer holds near, faster the deeper it holds, and
    /// follows the items that pass under the lifted copy or grow beside it while it holds still.
    fn autoscroll(&mut self, items: &ScrollHandle, ids: &[Id], window: &mut Window, cx: &App) {
        let axis = self.axis;
        let Some(lift) = self.lift.as_mut() else {
            return;
        };
        let Some(pointer) = lift.pointer else {
            return;
        };
        // A held gap stays held as the strip follows the pointer.
        let gap_held = lift.gap_held;
        let viewport = items.bounds();
        let start = f32::from(viewport.origin.along(axis));
        let velocity = autoscroll_velocity(
            (start, start + f32::from(viewport.size.along(axis))),
            f32::from(pointer.along(axis)),
        );
        let offset = items.offset();
        let current = f32::from(offset.along(axis));
        let limit = -f32::from(items.max_offset().along(axis));
        if !((velocity > 0.0 && current < 0.0) || (velocity < 0.0 && current > limit)) {
            lift.scrolled_at = None;
            self.follow(items, ids, pointer, gap_held, cx);
            return;
        }
        let now = now(cx);
        let elapsed = lift
            .scrolled_at
            .map_or(Duration::ZERO, |at| now.saturating_duration_since(at))
            .min(AUTOSCROLL_STEP);
        lift.scrolled_at = Some(now);
        let next = (current + velocity * elapsed.as_secs_f32()).clamp(limit, 0.0);
        items.set_offset(offset.apply_along(axis, |_| px(next)));
        window.request_animation_frame();
        self.follow(items, ids, pointer, gap_held, cx);
    }

    /// Ends the drag on its release. Returns the dragged item and its new position among the
    /// strip's `ids` when it lands somewhere new; the item then settles there.
    pub(crate) fn finish(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        cx: &App,
    ) -> Option<(Id, usize)> {
        let lift = self.lift.take()?;
        if !lift.session.is_active(cx) {
            self.slides.clear();
            return None;
        }
        let landing = lift
            .landing
            .filter(|landing| landing.len == ids.len())
            .map(|landing| landing.position);
        self.settle(&lift, landing, items, ids, cx)
            .map(|position| (lift.id, position))
    }

    /// Ends the drag without moving anything. The lifted item and the items it pushed aside
    /// return to their places.
    pub(crate) fn cancel(&mut self, items: &ScrollHandle, ids: &[Id], cx: &App) {
        if let Some(lift) = self.lift.take() {
            self.settle(&lift, None, items, ids, cx);
        }
    }

    /// Slides every item toward the place that opens the gap at the current landing, or back to
    /// rest when there is none. The lifted item's empty place moves into the gap.
    fn open_gap(&mut self, ids: &[Id], bounds: &[Bounds<Pixels>], cx: &App) {
        let Some(lift) = self.lift.as_ref() else {
            return;
        };
        let lifted = lift.id;
        let shifts = lift
            .landing
            .zip(ids.iter().position(|candidate| *candidate == lifted))
            .filter(|_| bounds.len() == ids.len())
            .map(|(landing, current)| {
                gap_shifts(&spans(self.axis, bounds), current, landing.position)
            })
            .unwrap_or_else(|| vec![0.0; ids.len()]);
        let now = now(cx);
        let motion = super::appearance_runtime::control_motion(cx);
        for (id, shift) in ids.iter().zip(shifts) {
            let to = Point::default().apply_along(self.axis, |_| px(shift));
            self.slide(*id, to, SHIFT_DURATION, now, motion);
        }
    }

    /// Lands the lifted item at `landing`, or returns it to its place, from wherever its lifted
    /// copy stands. Returns the landing when it is a new position.
    fn settle(
        &mut self,
        lift: &Lift<Id>,
        landing: Option<usize>,
        items: &ScrollHandle,
        ids: &[Id],
        cx: &App,
    ) -> Option<usize> {
        let faces = self.item_bounds(items, ids);
        let slots = painted_item_bounds(items);
        let Some(current) = ids
            .iter()
            .position(|candidate| *candidate == lift.id)
            .filter(|_| faces.len() == ids.len() && slots.len() == ids.len())
        else {
            self.slides.clear();
            return None;
        };
        let landing = landing.filter(|position| *position != current);
        let now = now(cx);
        let motion = super::appearance_runtime::control_motion(cx);
        let spans = spans(self.axis, &slots);
        match landing {
            // Each item keeps easing toward the gap it was opening, now its laid-out place. One
            // whose gap no longer matches the layout eases from where it stands to its place.
            Some(position) => {
                for (id, shift) in ids.iter().zip(gap_shifts(&spans, current, position)) {
                    let shift = Point::default().apply_along(self.axis, |_| px(shift));
                    if let Some(slide) = self.slides.get_mut(id) {
                        let rebased = slide.rebased(shift);
                        *slide = if rebased.to == Point::default() {
                            rebased
                        } else {
                            Slide::new(
                                rebased.at(now),
                                Point::default(),
                                SETTLE_DURATION,
                                now,
                                motion,
                            )
                        };
                    }
                }
            }
            None => {
                for id in ids {
                    self.slide(*id, Point::default(), SETTLE_DURATION, now, motion);
                }
            }
        }
        let start = landing.map_or(spans[current].0, |position| {
            landed_start(&spans, current, position)
        });
        // The face sits past the closing gap its slot holds before it.
        let lead = faces[current].origin.along(self.axis) - slots[current].origin.along(self.axis);
        let place = faces[current]
            .origin
            .apply_along(self.axis, |_| px(start) + lead);
        let from = lift
            .origin
            .map_or_else(Point::default, |origin| origin - place);
        self.slides.insert(
            lift.id,
            Slide::new(from, Point::default(), SETTLE_DURATION, now, motion),
        );
        self.settling = Some(lift.id);
        landing
    }

    /// Eases the item `id` from where it stands toward `to`, unless it is already headed there.
    fn slide(
        &mut self,
        id: Id,
        to: Point<Pixels>,
        duration: Duration,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) {
        let slide = self.slides.get(&id);
        if slide.map_or(Point::default(), |slide| slide.to) == to {
            return;
        }
        let from = slide.map_or_else(Point::default, |slide| slide.at(now));
        self.slides
            .insert(id, Slide::new(from, to, duration, now, motion));
    }
}

/// Where a lifted copy whose item rests at `rest` stands when its origin follows the pointer to
/// `origin`: level with the items across the strip, and between the ends of its `viewport` along it.
fn confine(
    axis: Axis,
    viewport: Bounds<Pixels>,
    rest: Bounds<Pixels>,
    origin: Point<Pixels>,
) -> Point<Pixels> {
    let start = viewport.origin.along(axis);
    let end = (start + viewport.size.along(axis) - rest.size.along(axis)).max(start);
    let along = origin.along(axis).max(start).min(end);
    rest.origin.apply_along(axis, |_| along)
}

/// How fast a strip spanning `viewport` along its axis scrolls with the pointer at `pointer`, in
/// points per second of scroll offset. Near the start the offset grows toward zero, showing
/// earlier items; near the end it shrinks, showing later ones.
fn autoscroll_velocity(viewport: (f32, f32), pointer: f32) -> f32 {
    let (start, end) = viewport;
    let edge = AUTOSCROLL_EDGE.min((end - start) / 2.0);
    if edge <= 0.0 {
        return 0.0;
    }
    let depth = |distance: f32| ((edge - distance) / edge).clamp(0.0, 1.0);
    AUTOSCROLL_SPEED * (depth(pointer - start) - depth(end - pointer))
}

/// The position a lifted item whose lifted copy has its center at `center` lands in: past every
/// other item whose midpoint the center has passed. Reaching a midpoint passes the item.
fn landing_position(spans: &[(f32, f32)], dragged: usize, center: f32) -> usize {
    spans
        .iter()
        .enumerate()
        .filter(|(index, (start, end))| {
            let midpoint = (start + end) / 2.0;
            (*index < dragged && center > midpoint) || (*index > dragged && center >= midpoint)
        })
        .count()
}

/// The position among items at `spans` where an arriving item lands at `along`: after every item
/// whose middle lies before it.
fn incoming_position(spans: &[(f32, f32)], along: f32) -> usize {
    spans
        .iter()
        .filter(|(start, end)| along > (start + end) / 2.0)
        .count()
}

/// The item, other than the one lifted from `dragged`, whose span, drawn shifted for the gap at
/// `landing`, holds `center` within its middle `share`.
fn covered_position(
    spans: &[(f32, f32)],
    dragged: usize,
    landing: usize,
    center: f32,
    share: f32,
) -> Option<usize> {
    let shifts = gap_shifts(spans, dragged, landing);
    spans
        .iter()
        .zip(shifts)
        .enumerate()
        .filter(|(index, _)| *index != dragged)
        .find(|(_, ((start, end), shift))| {
            let inset = (end - start) * (1.0 - share) / 2.0;
            (start + shift + inset..=end + shift - inset).contains(&center)
        })
        .map(|(index, _)| index)
}

/// How far each item moves along the strip to open a gap at `landing` for the item lifted from
/// `dragged`. Each moved item takes the place its neighbor will leave, and the lifted item's empty
/// place moves into the gap.
fn gap_shifts(spans: &[(f32, f32)], dragged: usize, landing: usize) -> Vec<f32> {
    let toward_start = spans
        .get(dragged + 1)
        .map_or(0.0, |next| next.0 - spans[dragged].0);
    let toward_end = dragged
        .checked_sub(1)
        .map_or(0.0, |previous| spans[dragged].1 - spans[previous].1);
    (0..spans.len())
        .map(|index| {
            if index == dragged {
                landed_start(spans, dragged, landing) - spans[dragged].0
            } else if dragged < landing && index > dragged && index <= landing {
                -toward_start
            } else if landing < dragged && index >= landing && index < dragged {
                toward_end
            } else {
                0.0
            }
        })
        .collect()
}

/// Where the leading edge of the item lifted from `dragged` rests once it lands at `landing`.
fn landed_start(spans: &[(f32, f32)], dragged: usize, landing: usize) -> f32 {
    let (start, end) = spans[dragged];
    match landing.cmp(&dragged) {
        std::cmp::Ordering::Greater => spans[landing].1 - (end - start),
        std::cmp::Ordering::Less => spans[landing].0,
        std::cmp::Ordering::Equal => start,
    }
}

/// The start and end of every item along the strip.
fn spans(axis: Axis, bounds: &[Bounds<Pixels>]) -> Vec<(f32, f32)> {
    bounds
        .iter()
        .map(|bounds| {
            let start = bounds.origin.along(axis);
            (f32::from(start), f32::from(start + bounds.size.along(axis)))
        })
        .collect()
}

/// Opts a test into the motion of items entering and leaving a strip. Other tests see every item
/// at rest as soon as it is added or removed.
#[cfg(test)]
pub(crate) struct AnimatePresence;

#[cfg(test)]
impl gpui::Global for AnimatePresence {}

pub(super) fn presence_motion(cx: &App) -> spaceterm_ui::ControlMotion {
    #[cfg(test)]
    if !cx.has_global::<AnimatePresence>() {
        return spaceterm_ui::ControlMotion::Reduced;
    }
    super::appearance_runtime::control_motion(cx)
}

/// A progress from 0 to 1 that starts at `since`, fast at first and slowing to a stop, or, for a
/// closing place, slow at first.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ease {
    since: Instant,
    duration: Duration,
    curve: Curve,
}

/// How an [`Ease`] moves along its duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Curve {
    /// Fast at first, slowing to a stop: [`decay`].
    Decay,
    /// Slow at first, fastest midway, slowing to a stop: [`settle`].
    Settle,
}

impl Ease {
    /// An ease that stands `eased` of the way along at `now`.
    fn reached(
        eased: f32,
        duration: Duration,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) -> Self {
        let ease = Self::new(duration, now, motion);
        let elapsed = ease.duration.mul_f32(undecay(eased));
        Self {
            since: now.checked_sub(elapsed).unwrap_or(now),
            ..ease
        }
    }

    /// An ease that starts at `now`, or ends at once when motion is reduced.
    pub(super) fn new(
        duration: Duration,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) -> Self {
        Self {
            since: now,
            duration: if motion == spaceterm_ui::ControlMotion::Reduced {
                Duration::ZERO
            } else {
                duration
            },
            curve: Curve::Decay,
        }
    }

    /// How the place an item leaves at `now` closes over [`CLOSING_DURATION`], or at once when
    /// motion is reduced.
    fn closing(now: Instant, motion: spaceterm_ui::ControlMotion) -> Self {
        Self {
            curve: Curve::Settle,
            ..Self::new(CLOSING_DURATION, now, motion)
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.since).as_secs_f32() / self.duration.as_secs_f32())
            .min(1.0)
    }

    pub(super) fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    pub(super) fn at(&self, now: Instant) -> f32 {
        let progress = self.progress(now);
        match self.curve {
            Curve::Decay => decay(progress),
            Curve::Settle => settle(progress),
        }
    }
}

/// The space removed items left in the strip. Each removal's place closes on its own from the
/// moment it left, so a removal that joins a gap already closing leaves the rest of it moving as
/// it was.
#[derive(Clone, Debug)]
struct Gap<Id> {
    places: Vec<Closing>,
    /// The removed items whose place the gap holds, in presentation order, which a strip may
    /// draw in it while it closes.
    leaving: Vec<Leaving<Id>>,
}

/// One removal's place in a [`Gap`], closing from `extent` to nothing.
#[derive(Clone, Copy, Debug)]
struct Closing {
    extent: Pixels,
    ease: Ease,
}

/// A removed item a closing gap holds, and how much of it shows from its start: all of it when
/// it left, or what showed when the gap that held it joined another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Leaving<Id> {
    /// The item, or `None` once it returned to the strip, which leaves its place here empty.
    pub(crate) id: Option<Id>,
    pub(crate) extent: Pixels,
    /// When the item left the strip.
    left_at: Instant,
    /// How far the item had grown in as it left, if it was still growing.
    entering: Option<f32>,
    /// Whether the item was growing from its slot's far end as it left.
    grows_from_end: bool,
}

impl<Id> Leaving<Id> {
    /// How the removed item presents what it showed as it left: as much of its growth as it
    /// had made, from the end it grew from.
    pub(crate) fn motion(&self) -> ItemMotion {
        ItemMotion {
            entering: self.entering,
            grows_from_end: self.grows_from_end,
            ..ItemMotion::default()
        }
    }

    /// How opaque the removed item draws where it left, as it fades before its place closes.
    pub(crate) fn opacity(&self, cx: &App) -> f32 {
        if presence_motion(cx) == spaceterm_ui::ControlMotion::Reduced {
            return 0.0;
        }
        self.opacity_at(now(cx))
    }

    /// How opaque the removed item drew at `now`.
    pub(crate) fn opacity_at(&self, now: Instant) -> f32 {
        let faded =
            now.saturating_duration_since(self.left_at).as_secs_f32() / LEAVING_FADE.as_secs_f32();
        1.0 - faded.min(1.0)
    }
}

/// What of `leaving`, drawn one after another from a gap's start, shows in `open` of it.
fn shown_leaving<Id>(leaving: Vec<Leaving<Id>>, open: Pixels) -> Vec<Leaving<Id>> {
    let mut remaining = open;
    leaving
        .into_iter()
        .filter_map(|leaving| {
            let extent = leaving.extent.min(remaining);
            remaining -= extent;
            (extent > px(0.0)).then_some(Leaving { extent, ..leaving })
        })
        .collect()
}

impl<Id> Default for Gap<Id> {
    fn default() -> Self {
        Self {
            places: Vec::new(),
            leaving: Vec::new(),
        }
    }
}

impl<Id> Gap<Id> {
    /// A gap with one place, closing from `extent` on `ease`.
    fn closing(extent: Pixels, ease: Ease) -> Self {
        Self {
            places: vec![Closing { extent, ease }],
            leaving: Vec::new(),
        }
    }

    /// How long the gap is at `now`.
    fn at(&self, now: Instant) -> Pixels {
        self.places.iter().fold(px(0.0), |open, place| {
            open + place.extent * (1.0 - place.ease.at(now))
        })
    }

    /// Whether every place in the gap has closed by `now`.
    fn closed(&self, now: Instant) -> bool {
        self.places.iter().all(|place| place.ease.finished(now))
    }
}

/// One item's displacement easing from `from` to `to`.
#[derive(Clone, Copy, Debug)]
struct Slide {
    from: Point<Pixels>,
    to: Point<Pixels>,
    since: Instant,
    duration: Duration,
}

impl Slide {
    /// A slide that starts at `now`, or lands at once when motion is reduced.
    fn new(
        from: Point<Pixels>,
        to: Point<Pixels>,
        duration: Duration,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) -> Self {
        Self {
            from,
            to,
            since: now,
            duration: if motion == spaceterm_ui::ControlMotion::Reduced {
                Duration::ZERO
            } else {
                duration
            },
        }
    }

    fn progress(&self, now: Instant) -> f32 {
        if self.duration.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.since).as_secs_f32() / self.duration.as_secs_f32())
            .min(1.0)
    }

    fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    fn at(&self, now: Instant) -> Point<Pixels> {
        let eased = decay(self.progress(now));
        point(
            self.from.x + (self.to.x - self.from.x) * eased,
            self.from.y + (self.to.y - self.from.y) * eased,
        )
    }

    /// The same slide, measured from a laid-out place that moved by `by`.
    fn rebased(self, by: Point<Pixels>) -> Self {
        Self {
            from: self.from - by,
            to: self.to - by,
            ..self
        }
    }
}

/// Closes the same share of the remaining distance in each moment, so it starts at full speed
/// and slows toward the end, without overshoot.
/// How many time constants the motion runs, by when less than 1% of the distance remains.
const TIME_CONSTANTS: f32 = 6.2;

fn decay(progress: f32) -> f32 {
    (1.0 - (-TIME_CONSTANTS * progress).exp()) / (1.0 - (-TIME_CONSTANTS).exp())
}

/// Starts slowly, moves fastest midway and slows to a stop, symmetrically: a cubic ease in and out.
fn settle(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    if progress < 0.5 {
        4.0 * progress.powi(3)
    } else {
        1.0 - (2.0 - 2.0 * progress).powi(3) / 2.0
    }
}

/// The progress at which [`decay`] reaches `eased`, from 0 to 1.
fn undecay(eased: f32) -> f32 {
    let eased = eased.clamp(0.0, 1.0);
    (-(1.0 - eased * (1.0 - (-TIME_CONSTANTS).exp())).ln() / TIME_CONSTANTS).clamp(0.0, 1.0)
}

pub(super) fn now(cx: &App) -> Instant {
    cx.background_executor().now()
}

/// The shadow under a lifted item's chip, `level` of the way from resting to lifted.
pub(crate) fn lift_shadow(
    appearance: &super::appearance::ChromeAppearance,
    level: f32,
) -> Vec<BoxShadow> {
    if level <= 0.0 {
        return Vec::new();
    }
    let layer = |share: f32, offset_y: f32, blur: f32| {
        let opacity = (share * level.min(1.0) * 255.0).round() as u8;
        BoxShadow {
            inset: false,
            color: gpui_color(appearance.colors.shadow.multiply_opacity(opacity)).into(),
            offset: point(px(0.0), px(offset_y)),
            blur_radius: px(blur),
            spread_radius: px(0.0),
        }
    };
    vec![layer(1.0, 10.0, 28.0), layer(0.7, 2.0, 6.0)]
}

/// How opaque an item is that shows `lift` of its lift.
fn lifted_opacity(lift: f32) -> f32 {
    1.0 - (1.0 - LIFTED_OPACITY) * lift.clamp(0.0, 1.0)
}

/// Finishes every strip slide, growth, and closing gap in progress, since test windows have no
/// frame loop.
#[cfg(test)]
pub(crate) fn finish_slides(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor().advance_clock(
        SHIFT_DURATION
            .max(SETTLE_DURATION)
            .max(PRESENCE_DURATION)
            .max(CLOSING_DURATION)
            .max(SELECTION_DURATION)
            .max(MORPH_DURATION),
    );
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

/// What the window painted in the last frame for the element whose debug bounds `selector`
/// names: the bounds of the quad drawn there, the content mask that clips it, and how opaque
/// its fill is, in window points.
#[cfg(test)]
pub(crate) fn painted_quad(
    selector: &'static str,
    cx: &mut gpui::VisualTestContext,
) -> Option<(Bounds<Pixels>, Bounds<Pixels>, f32)> {
    let element = cx.debug_bounds(selector)?;
    cx.update(|window, _| {
        let scale = window.scale_factor();
        let points = |bounds: Bounds<gpui::ScaledPixels>| {
            Bounds::new(
                point(px(bounds.origin.x.0 / scale), px(bounds.origin.y.0 / scale)),
                Size::new(
                    px(bounds.size.width.0 / scale),
                    px(bounds.size.height.0 / scale),
                ),
            )
        };
        window
            .painted_quads()
            .into_iter()
            .filter(|quad| {
                let bounds = points(quad.bounds);
                (bounds.origin.x - element.origin.x).abs() < px(1.0)
                    && (bounds.origin.y - element.origin.y).abs() < px(1.0)
                    && (bounds.size.width - element.size.width).abs() < px(1.0)
                    && (bounds.size.height - element.size.height).abs() < px(1.0)
            })
            .find_map(|quad| {
                let fill = quad.background.as_solid()?;
                Some((
                    points(quad.bounds),
                    points(quad.content_mask.bounds),
                    fill.a,
                ))
            })
    })
}

/// Whether `inner` lies within `outer`, give or take half a point of rounding.
#[cfg(test)]
pub(crate) fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left() - px(0.5)
        && inner.right() <= outer.right() + px(0.5)
        && inner.top() >= outer.top() - px(0.5)
        && inner.bottom() <= outer.bottom() + px(0.5)
}

/// The bounds a scroll container painted its items at, in presentation order.
fn painted_item_bounds(items: &ScrollHandle) -> Vec<Bounds<Pixels>> {
    let offset = items.offset();
    (0..items.children_count())
        .filter_map(|position| items.bounds_for_item(position))
        .map(|bounds| Bounds::new(bounds.origin + offset, bounds.size))
        .collect()
}

/// How long a drag picture takes to change from one shape to another.
const MORPH_DURATION: Duration = Duration::from_millis(180);

/// Eases a drag picture's bounds when what it shows changes, as a lifted Tab that turns into the
/// rectangle a Pane drag carries and back, around the pointer.
pub(crate) struct FaceMorph<Kind> {
    /// What the picture showed in the last frame, and where.
    shown: Option<(Kind, Bounds<Pixels>)>,
    /// What it shows now is turning from, from where, and since when.
    from: Option<(Kind, Bounds<Pixels>, Ease)>,
}

/// One frame of a [`FaceMorph`].
#[derive(Clone, Copy, Debug)]
pub(crate) struct Morph<Kind> {
    pub(crate) bounds: Bounds<Pixels>,
    /// What the picture is turning from, and how far along the turn is, from 0 to 1.
    pub(crate) from: Option<(Kind, f32)>,
}

impl<Kind: Copy + PartialEq> FaceMorph<Kind> {
    pub(crate) fn new() -> Self {
        Self {
            shown: None,
            from: None,
        }
    }

    /// Where to draw `kind` this frame, which rests at `target`.
    pub(crate) fn frame(&mut self, kind: Kind, target: Bounds<Pixels>, cx: &App) -> Morph<Kind> {
        let now = now(cx);
        if let Some((shown, drawn)) = self.shown
            && shown != kind
        {
            let motion = super::appearance_runtime::control_motion(cx);
            self.from = Some((shown, drawn, Ease::new(MORPH_DURATION, now, motion)));
        }
        let morph = match self.from {
            Some((from, start, ease)) if !ease.finished(now) => {
                let t = ease.at(now);
                let lerp = |a: Pixels, b: Pixels| a + (b - a) * t;
                Morph {
                    bounds: Bounds::new(
                        point(
                            lerp(start.origin.x, target.origin.x),
                            lerp(start.origin.y, target.origin.y),
                        ),
                        Size::new(
                            lerp(start.size.width, target.size.width),
                            lerp(start.size.height, target.size.height),
                        ),
                    ),
                    from: Some((from, t)),
                }
            }
            _ => {
                self.from = None;
                Morph {
                    bounds: target,
                    from: None,
                }
            }
        };
        self.shown = Some((kind, morph.bounds));
        morph
    }
}

/// The lifted copy of a dragged item that follows the pointer.
pub(crate) struct DragPreview {
    /// How far the pointer moved between the press and the motion that started the drag. GPUI
    /// places the preview from that motion, so the face moves back by this much.
    lift: Point<Pixels>,
    face: Box<PreviewFace>,
    placement: Option<Placement>,
}

/// Paints a dragged item's face for one frame.
type PreviewFace = dyn Fn(&mut Window, &mut App) -> AnyElement;

/// Where a preview's owner stands the face, instead of at the pointer.
struct Placement {
    /// Where GPUI places the preview from the pointer, as it reported when the drag started.
    cursor_offset: Point<Pixels>,
    origin: Box<PreviewOrigin>,
}

/// The face's origin in the window for the pointer at the given point and the window's current
/// size, while the owner places it.
type PreviewOrigin = dyn Fn(Point<Pixels>, Size<Pixels>, &App) -> Option<Point<Pixels>>;

impl DragPreview {
    /// A preview, for the drag GPUI is starting in `window`, that paints `face` at the pointer.
    /// The face must not claim the pointer: it lies over every drop target while the drag lasts.
    pub(crate) fn new(
        window: &Window,
        cx: &App,
        face: impl Fn(&mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        Self {
            lift: window.mouse_position() - grab_point(window, cx),
            face: Box::new(face),
            placement: None,
        }
    }

    /// Stands the face at the origin `origin` reports, such as level with a strip, and at the
    /// pointer while it reports none. `cursor_offset` is the offset GPUI passed to the drag's
    /// constructor.
    pub(crate) fn placed(
        mut self,
        cursor_offset: Point<Pixels>,
        origin: impl Fn(Point<Pixels>, Size<Pixels>, &App) -> Option<Point<Pixels>> + 'static,
    ) -> Self {
        self.placement = Some(Placement {
            cursor_offset,
            origin: Box::new(origin),
        });
        self
    }

    /// A preview with nothing to paint, for a drag whose owner is gone.
    pub(crate) fn empty() -> Self {
        Self {
            lift: Point::default(),
            face: Box::new(|_, _| div().into_any_element()),
            placement: None,
        }
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI lays the preview out as a root, which ignores its own offset, so the face moves.
        let offset = self
            .placement
            .as_ref()
            .and_then(|placement| {
                let pointer = window.mouse_position();
                let root = pointer - placement.cursor_offset;
                (placement.origin)(pointer, window.viewport_size(), cx).map(|origin| origin - root)
            })
            .unwrap_or(self.lift);
        div().debug_selector(|| "drag-preview".to_owned()).child(
            div()
                .relative()
                .left(offset.x)
                .top(offset.y)
                .child((self.face)(window, cx)),
        )
    }
}

/// Where the press that a drag starting in `window` began from took the pointer. GPUI starts a drag
/// on the first motion past a threshold, which can land well past the press.
pub(crate) fn grab_point(window: &Window, cx: &App) -> Point<Pixels> {
    let window_id = window.window_handle().window_id();
    cx.try_global::<LastPress>()
        .filter(|press| press.window_id == window_id)
        .map_or_else(|| window.mouse_position(), |press| press.position)
}

/// The latest primary-button press in any window that mounts a drag release observer.
struct LastPress {
    window_id: WindowId,
    position: Point<Pixels>,
}

impl gpui::Global for LastPress {}

/// Calls `on_release` when a button is released anywhere in the window while a drag is active, and
/// records each press a drag could start from.
///
/// GPUI ends every drag on any release but tells only the target. Mount this whether or not a drag
/// is in progress, because GPUI dispatches a release to the last painted frame's listeners.
pub(crate) fn drag_release_observer(
    on_release: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            window.on_mouse_event(|event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    cx.set_global(LastPress {
                        window_id: window.window_handle().window_id(),
                        position: event.position,
                    });
                }
            });
            let on_release = on_release.clone();
            window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && cx.has_active_drag() {
                    on_release(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
}

/// The most recent drag a [`DragSession`] began in this application, and how Escape cancels it.
#[derive(Default)]
struct CurrentDrag {
    issued: u64,
    current: Option<u64>,
    on_cancel: Option<Rc<CancelDrag>>,
    escape: Option<Subscription>,
}

impl gpui::Global for CurrentDrag {}

/// Tells the owner of a drag that Escape cancelled it.
type CancelDrag = dyn Fn(&mut Window, &mut App);

/// One drag that one owner started. While GPUI carries it, Escape in any window cancels it before
/// the key reaches any focused element, including a Terminal Session.
pub(crate) struct DragSession {
    ticket: u64,
}

impl DragSession {
    /// Begins a session for the drag GPUI is starting. Escape stops that drag and then calls
    /// `on_cancel` on the owner, if the owner still exists.
    pub(crate) fn begin<T: 'static>(
        cx: &mut Context<T>,
        on_cancel: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    ) -> Self {
        let owner = cx.weak_entity();
        let on_cancel: Rc<CancelDrag> = Rc::new(move |window, cx| {
            let _ = owner.update(cx, |owner, cx| on_cancel(owner, window, cx));
        });
        if cx
            .try_global::<CurrentDrag>()
            .is_none_or(|drags| drags.escape.is_none())
        {
            let escape = cx.intercept_keystrokes(cancel_on_escape);
            cx.default_global::<CurrentDrag>().escape = Some(escape);
        }
        let drags = cx.global_mut::<CurrentDrag>();
        drags.issued += 1;
        drags.current = Some(drags.issued);
        drags.on_cancel = Some(on_cancel);
        Self {
            ticket: drags.issued,
        }
    }

    pub(crate) fn is_active(&self, cx: &App) -> bool {
        carries(self.ticket, cx)
    }
}

fn cancel_on_escape(event: &KeystrokeEvent, window: &mut Window, cx: &mut App) {
    if event.keystroke.key != "escape" {
        return;
    }
    let Some(on_cancel) = cx
        .try_global::<CurrentDrag>()
        .and_then(|drags| drags.on_cancel.clone())
    else {
        return;
    };
    if !cx.stop_active_drag(window) {
        return;
    }
    cx.stop_propagation();
    on_cancel(window, cx);
}

fn carries(ticket: u64, cx: &App) -> bool {
    cx.has_active_drag()
        && cx
            .try_global::<CurrentDrag>()
            .is_some_and(|drags| drags.current == Some(ticket))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, point};

    #[gpui::test]
    fn taking_another_place_should_keep_a_strip_joinable(cx: &mut TestAppContext) {
        let mut strip = ReorderableStrip::<u64>::joinable(Axis::Horizontal);

        cx.update(|cx| strip.take_place(&ScrollHandle::new(), &[1, 2], &[], cx));

        assert!(
            strip.joinable,
            "another Worktree's Tabs still land by pointer and join each other"
        );
    }

    struct Payload;

    /// Owns the drag its root's source starts, as a Tab, Workspace sidebar, or Tab view does.
    #[derive(Default)]
    struct Owner {
        session: Option<DragSession>,
        cancelled: usize,
    }

    struct Root {
        owner: Option<Entity<Owner>>,
    }

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let owner = self.owner.clone();
            div()
                .size_full()
                .child(drag_release_observer(|_, _| {}))
                .child(
                    div()
                        .id("source")
                        .debug_selector(|| "drag-source".to_owned())
                        .size(px(40.0))
                        .on_drag(Payload, move |_, _, window, cx| {
                            if let Some(owner) = &owner {
                                owner.update(cx, |owner, cx| {
                                    owner.session =
                                        Some(DragSession::begin(cx, |owner: &mut Owner, _, _| {
                                            owner.cancelled += 1
                                        }));
                                });
                            }
                            {
                                let preview = DragPreview::new(window, cx, |_, _| {
                                    div()
                                        .debug_selector(|| "drag-face".to_owned())
                                        .size(px(40.0))
                                        .into_any_element()
                                });
                                cx.new(|_| preview)
                            }
                        }),
                )
        }
    }

    fn drag_from_source(cx: &mut VisualTestContext) {
        let source = cx.debug_bounds("drag-source").unwrap();
        cx.simulate_mouse_move(source.center(), None, Modifiers::none());
        cx.simulate_mouse_down(source.center(), MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            source.center() + point(px(8.0), px(0.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();
    }

    #[gpui::test]
    fn escape_should_cancel_a_drag_whose_owner_is_gone(cx: &mut TestAppContext) {
        let (root, cx) = cx.add_window_view(|_, cx| Root {
            owner: Some(cx.new(|_| Owner::default())),
        });
        cx.run_until_parked();
        drag_from_source(cx);
        assert!(cx.update(|_, cx| cx.has_active_drag()));

        root.update(cx, |root, cx| {
            root.owner = None;
            cx.notify();
        });
        cx.run_until_parked();
        cx.simulate_keystrokes("escape");

        assert!(!cx.update(|_, cx| cx.has_active_drag()));
    }

    #[gpui::test]
    fn escape_in_another_window_should_cancel_the_drag(cx: &mut TestAppContext) {
        let owner = cx.new(|_| Owner::default());
        let source_owner = owner.clone();
        let source = cx.add_window(move |_, _| Root {
            owner: Some(source_owner),
        });
        let other = cx.add_window(|_, _| Root { owner: None });
        let mut source = VisualTestContext::from_window(source.into(), cx);
        source.run_until_parked();
        drag_from_source(&mut source);

        let mut other = VisualTestContext::from_window(other.into(), cx);
        other.simulate_keystrokes("escape");

        assert!(!other.update(|_, cx| cx.has_active_drag()));
        assert_eq!(owner.read_with(cx, |owner, _| owner.cancelled), 1);
    }

    #[gpui::test]
    fn a_preview_should_keep_the_press_point_under_the_pointer(cx: &mut TestAppContext) {
        let (_, cx) = cx.add_window_view(|_, cx| Root {
            owner: Some(cx.new(|_| Owner::default())),
        });
        cx.run_until_parked();
        let source = cx.debug_bounds("drag-source").unwrap();
        let press = source.origin + point(px(10.0), px(10.0));
        let far = press + point(px(200.0), px(120.0));

        cx.simulate_mouse_move(press, None, Modifiers::none());
        cx.simulate_mouse_down(press, MouseButton::Left, Modifiers::none());
        // The first motion GPUI delivers lands far past the press.
        cx.simulate_mouse_move(far, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        cx.simulate_mouse_move(
            far + point(px(1.0), px(0.0)),
            MouseButton::Left,
            Modifiers::none(),
        );
        cx.run_until_parked();

        let face = cx.debug_bounds("drag-face").unwrap();
        assert_eq!(
            face.origin,
            far + point(px(1.0), px(0.0)) - (press - source.origin)
        );
    }

    fn strip(extents: &[f32]) -> Vec<(f32, f32)> {
        let mut start = 0.0;
        extents
            .iter()
            .map(|extent| {
                let span = (start, start + extent);
                start += extent;
                span
            })
            .collect()
    }

    #[test]
    fn landing_position_should_follow_each_item_whose_midpoint_the_lifted_center_passed() {
        let spans = strip(&[100.0, 100.0, 100.0, 100.0]);

        // B is lifted from A B C D.
        assert_eq!(
            [
                landing_position(&spans, 1, 40.0),
                landing_position(&spans, 1, 90.0),
                landing_position(&spans, 1, 210.0),
                landing_position(&spans, 1, 251.0),
                landing_position(&spans, 1, 350.0),
                // A held at the leading end of the strip stays first.
                landing_position(&spans, 0, 50.0),
                // A copy carried past either end lands at that end.
                landing_position(&spans, 1, -450.0),
                landing_position(&spans, 1, 950.0),
            ],
            [0, 1, 1, 2, 3, 0, 0, 3]
        );
    }

    #[test]
    fn gap_shifts_should_move_each_passed_item_into_its_neighbors_place() {
        // A narrow B sits between wider items, so each direction moves by B's own extent, and B's
        // empty place moves to where it would land.
        let spans = strip(&[100.0, 60.0, 100.0, 100.0]);

        assert_eq!(
            [
                gap_shifts(&spans, 1, 3),
                gap_shifts(&spans, 1, 0),
                gap_shifts(&spans, 1, 1),
            ],
            [
                vec![0.0, 200.0, -60.0, -60.0],
                vec![60.0, -100.0, 0.0, 0.0],
                vec![0.0; 4],
            ]
        );
    }

    #[test]
    fn covered_position_should_find_the_item_whose_drawn_middle_holds_the_center() {
        let spans = strip(&[100.0, 100.0, 100.0, 100.0]);

        // B is lifted from A B C D. With no gap moved, C is drawn in its own place; once the gap
        // moves past C, C is drawn in B's place.
        assert_eq!(
            [
                covered_position(&spans, 1, 1, 250.0, 0.5),
                covered_position(&spans, 1, 1, 230.0, 0.5),
                covered_position(&spans, 1, 1, 220.0, 0.5),
                covered_position(&spans, 1, 1, 50.0, 0.5),
                covered_position(&spans, 1, 1, 150.0, 0.5),
                covered_position(&spans, 1, 2, 150.0, 0.5),
            ],
            [Some(2), Some(2), None, Some(0), None, Some(2)]
        );
    }

    #[test]
    fn landed_start_should_be_the_gap_the_shifted_items_leave() {
        let spans = strip(&[100.0, 60.0, 100.0, 100.0]);

        assert_eq!(
            [
                landed_start(&spans, 1, 3),
                landed_start(&spans, 1, 0),
                landed_start(&spans, 1, 1),
            ],
            [300.0, 0.0, 100.0]
        );
    }

    #[test]
    fn autoscroll_velocity_should_grow_toward_either_end_of_the_strip() {
        let viewport = (100.0, 500.0);

        assert_eq!(
            [
                autoscroll_velocity(viewport, 300.0),
                autoscroll_velocity(viewport, 140.0),
                autoscroll_velocity(viewport, 120.0),
                autoscroll_velocity(viewport, 60.0),
                autoscroll_velocity(viewport, 480.0),
                autoscroll_velocity(viewport, 540.0),
            ],
            [
                0.0,
                0.0,
                AUTOSCROLL_SPEED / 2.0,
                AUTOSCROLL_SPEED,
                -AUTOSCROLL_SPEED / 2.0,
                -AUTOSCROLL_SPEED,
            ]
        );
    }

    /// Renders a strip of `ids` whose selection rests on `selected`, as a strip's owner does.
    struct SelectingStrip {
        strip: ReorderableStrip<u64>,
        ids: Vec<u64>,
        selected: u64,
    }

    impl Render for SelectingStrip {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.strip
                .refresh(&ScrollHandle::new(), &self.ids, window, cx);
            self.strip.follow_selection(Some(self.selected), window, cx);
            div()
        }
    }

    /// A rendered strip of `ids` whose selection is on `selected`, drawn at `drawn`, in a window
    /// that shows presence motion.
    fn selecting_strip<'a>(
        ids: &[u64],
        selected: u64,
        drawn: Span,
        cx: &'a mut TestAppContext,
    ) -> (Entity<SelectingStrip>, &'a mut VisualTestContext) {
        cx.update(|cx| cx.set_global(AnimatePresence));
        let ids = ids.to_vec();
        let (view, cx) = cx.add_window_view(|_, _| SelectingStrip {
            strip: ReorderableStrip::new(Axis::Horizontal),
            ids,
            selected,
        });
        cx.run_until_parked();
        view.update(cx, |view, cx| {
            view.strip.drawn_selection.set(Some(DrawnSelection {
                span: drawn,
                at: now(cx),
                settled: true,
            }));
        });
        (view, cx)
    }

    /// Changes what the strip shows and selects, and draws the next frame.
    fn present(
        view: &Entity<SelectingStrip>,
        ids: &[u64],
        selected: u64,
        cx: &mut VisualTestContext,
    ) {
        view.update(cx, |view, cx| {
            view.ids = ids.to_vec();
            view.selected = selected;
            cx.notify();
        });
        cx.update(|window, cx| window.simulate_next_frame(cx));
        cx.run_until_parked();
    }

    #[gpui::test]
    fn a_selection_whose_item_left_should_fade_in_on_the_item_that_takes_it(
        cx: &mut TestAppContext,
    ) {
        let drawn = Span {
            start: px(100.0),
            extent: px(80.0),
        };
        let (view, cx) = selecting_strip(&[1, 2], 2, drawn, cx);
        let motion = |cx: &mut VisualTestContext| {
            view.read_with(cx, |view, cx| {
                let motion = view.strip.selection_motion(1, (), cx);
                (motion.glide.is_some(), motion.opacity)
            })
        };

        present(&view, &[1], 1, cx);
        let leaving = motion(cx);
        // The selection moves by the time each frame is prepared at.
        cx.executor().advance_clock(LEAVING_FADE / 2);
        present(&view, &[1], 1, cx);
        let fading = motion(cx);
        cx.executor().advance_clock(LEAVING_FADE);
        present(&view, &[1], 1, cx);
        let faded = motion(cx);

        assert_eq!(
            (leaving, faded),
            ((false, 0.0), (false, 1.0)),
            "the selection must fade in where it lands instead of gliding from the removed item"
        );
        assert!(
            (fading.1 - 0.5).abs() < 0.01,
            "the selection must be half faded in halfway through, got {fading:?}"
        );
    }

    #[gpui::test]
    fn a_selection_moving_between_items_should_glide_from_where_it_was_drawn(
        cx: &mut TestAppContext,
    ) {
        let drawn = Span {
            start: px(100.0),
            extent: px(80.0),
        };
        let (view, cx) = selecting_strip(&[1, 2], 2, drawn, cx);

        cx.executor().advance_clock(Duration::from_millis(500));
        present(&view, &[1, 2], 1, cx);
        let (gliding, opacity, from) = view.read_with(cx, |view, cx| {
            let motion = view.strip.selection_motion(1, (), cx);
            (motion.glide.is_some(), motion.opacity, motion.drawn.get())
        });

        assert_eq!(
            (gliding, opacity, from.map(|from| (from.span, from.settled))),
            (true, 1.0, Some((drawn, false))),
            "the selection must glide on from where it was drawn, starting now"
        );
        assert_eq!(
            from.map(|from| from.at),
            Some(cx.update(|_, cx| now(cx))),
            "the glide must start when the selection moves, not when it was last drawn"
        );
        // However long the frame takes to paint, the selection moves by the time its items were
        // prepared at, so it keeps pace with their growth.
        let prepared = cx.update(|_, cx| now(cx));
        cx.executor().advance_clock(Duration::from_millis(40));
        let painting_at = view.read_with(cx, |view, cx| view.strip.selection_motion(1, (), cx).at);
        assert_eq!(painting_at, prepared);
    }

    #[test]
    fn a_growing_items_parts_should_show_only_after_they_fit_and_fully_at_rest() {
        let growing = |presence| ItemMotion {
            entering: Some(presence),
            ..ItemMotion::default()
        };

        let reveals = [
            growing(0.5).reveal(0.6),
            growing(0.6).reveal(0.6),
            growing(0.6 + REVEAL_SPAN / 2.0).reveal(0.6),
            growing(0.6 + REVEAL_SPAN).reveal(0.6),
            // A part that fits only near the end is fully shown once the item rests.
            growing(0.99).reveal(0.98),
            ItemMotion::default().reveal(0.98),
        ];

        for (reveal, expected) in reveals.into_iter().zip([0.0, 0.0, 0.5, 1.0, 0.5, 1.0]) {
            assert!(
                (reveal - expected).abs() < 1e-3,
                "expected {expected}, got {reveals:?}"
            );
        }
    }

    #[test]
    fn undecay_should_find_the_progress_that_decay_reaches() {
        assert_eq!((undecay(0.0), undecay(1.0)), (0.0, 1.0));
        for progress in [0.1, 0.25, 0.5, 0.9] {
            assert!((undecay(decay(progress)) - progress).abs() < 1e-4);
        }
    }

    #[test]
    fn a_closing_place_should_move_at_once_yet_keep_its_fading_item_whole() {
        let faded = LEAVING_FADE.as_secs_f32() / CLOSING_DURATION.as_secs_f32();
        assert_eq!((settle(0.0), settle(1.0)), (0.0, 1.0));
        assert!(
            settle(faded / 4.0) > 0.0 && settle(faded) < 0.03,
            "the place closes at once, by under 3% while its item fades"
        );
        assert!((settle(0.5) - 0.5).abs() < 1e-6 && settle(0.75) < settle(0.9));
    }

    #[test]
    fn decay_should_start_at_full_speed_and_land_exactly() {
        assert_eq!((decay(0.0), decay(1.0)), (0.0, 1.0));
        assert!(decay(0.1) > 0.4, "the curve moves fastest at the start");
        assert!(decay(0.5) < decay(0.75) && decay(0.75) < 1.0);
    }
}
