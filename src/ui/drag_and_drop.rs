//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts, and the
//! motion curve their layout changes share.
//! A [`DragSession`] ties owner state to the one GPUI drag it started, so an unseen release cannot
//! strand it.

use gpui::prelude::*;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, BoxShadow, Context, DispatchPhase, Div,
    KeystrokeEvent, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollHandle, Size,
    Subscription, Window, WindowId, canvas, div, point, px,
};

use super::appearance::gpui_color;

/// How long the other items take to slide aside as the landing slot moves.
const SHIFT_DURATION: Duration = Duration::from_millis(310);
/// How long a released item takes to settle into its slot, and the others to follow it.
const SETTLE_DURATION: Duration = Duration::from_millis(440);
/// How long a new item takes to grow into the strip, and the gap a removed item leaves to close.
pub(super) const PRESENCE_DURATION: Duration = Duration::from_millis(440);
/// How long the selection takes to move from one item to another.
const SELECTION_DURATION: Duration = Duration::from_millis(310);
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
    /// How far the selected row stood from its item's start along the strip in the last frame.
    selected_inset: Pixels,
    /// Where the owner saw the selection drawn along the strip in the last frame, until the next
    /// selection change starts from there.
    drawn_selection: Option<Pixels>,
    /// The selection moving to the selected row.
    flight: Option<Flight<Id, Row>>,
    /// How far the strip was scrolled along its axis in the last frame.
    scrolled: Pixels,
    /// Whether the items of the last frame ran past the strip's visible end.
    overflowing: bool,
    /// Where the strip's visible part ended in the last frame, in layout coordinates.
    visible_end: Pixels,
    /// The shape of the strip this one takes the place of, until the next frame starts from it.
    arriving: Option<StripShape>,
    /// This strip moving from the shape of the strip it took the place of.
    arrival: Option<Arrival<Id>>,
}

/// Where a strip's items and selection stood in the window along its axis, so a strip that takes
/// its place can start from there.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct StripShape {
    /// Where each item started, and its size, in presentation order.
    slots: Vec<(Pixels, Pixels)>,
    /// Where the selection started.
    selection: Option<Pixels>,
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

/// The selection moving from where it was drawn to the row `row` of the item `id`.
struct Flight<Id, Row> {
    id: Id,
    row: Row,
    /// Where the selection started along the strip, from where the row settles.
    from: Pixels,
    ease: Ease,
    /// How far the strip was scrolled when the selection left, until the next layout shows
    /// whether revealing the selected item scrolled it.
    left_scrolled: Option<Pixels>,
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
    /// How far the item's selection is drawn from the item while it moves there.
    pub(crate) selection_offset: Point<Pixels>,
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
        self.follow_row_selection(selected.map(|id| (id, (), px(0.0))), window, cx);
    }
}

impl<Id: Copy + Eq + Hash, Row: Copy + Eq> ReorderableStrip<Id, Row> {
    pub(crate) fn new(axis: Axis) -> Self {
        Self {
            axis,
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
            selected_inset: px(0.0),
            drawn_selection: None,
            flight: None,
            scrolled: px(0.0),
            overflowing: false,
            visible_end: px(0.0),
            arriving: None,
            arrival: None,
        }
    }

    /// Where this strip's items and selection stand in the window, from the strip's `items` as
    /// last painted, or `None` while an item is lifted or the items do not match the strip.
    pub(crate) fn shape(&self, items: &ScrollHandle, cx: &App) -> Option<StripShape> {
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
        // The selection draws inside its item's face, which a drop may still be settling.
        let selection = self.selected.and_then(|(id, row)| {
            let position = ids.iter().position(|candidate| *candidate == id)?;
            let motion = self.motion(id, cx);
            let face = self.face(id, painted[position]).origin.along(axis);
            Some(
                face + motion.offset.along(axis)
                    + self.selected_inset
                    + self.row_selection_offset(id, row, cx).along(axis),
            )
        });
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
            .map(|(id, slot)| self.face(*id, slot))
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
            .map(|slot| self.face(id, slot).size)
    }

    /// The bounds of the item `id`'s face in `slot`, its slot in the last frame. A new item's face
    /// shows whole while its slot grows, from the slot's far end when it grows from there.
    fn face(&self, id: Id, slot: Bounds<Pixels>) -> Bounds<Pixels> {
        let item = self.trim(id, slot);
        let axis = self.axis;
        match self.entering.get(&id).and_then(|(_, natural)| *natural) {
            Some(natural) => {
                let lag = if self.growing_from_end.contains(&id) {
                    item.size.along(axis) - natural
                } else {
                    px(0.0)
                };
                Bounds::new(
                    item.origin.apply_along(axis, |start| start + lag),
                    item.size.apply_along(axis, |_| natural),
                )
            }
            None => item,
        }
    }

    /// Trims `slot`, the bounds of the item `id`'s slot in the last frame, to the item itself.
    fn trim(&self, id: Id, slot: Bounds<Pixels>) -> Bounds<Pixels> {
        let painted = |gaps: &HashMap<Id, Gap<Id>>| {
            self.refreshed_at
                .zip(gaps.get(&id))
                .map_or(px(0.0), |(at, gap)| gap.at(at))
        };
        let (lead, trail) = (painted(&self.leading_gaps), painted(&self.trailing_gaps));
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
                    None => self.laid_out.get(id).map(|(_, size)| *size),
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
                Gap {
                    extent: px(0.0),
                    ease: Ease::new(PRESENCE_DURATION, now, motion),
                    leaving: Vec::new(),
                },
            );
        }
        self.flight = self
            .selected
            .filter(|_| from.selection.is_some())
            .map(|(id, row)| Flight {
                id,
                row,
                from: px(0.0),
                ease: Ease::new(SELECTION_DURATION, now, motion),
                // The arrival aims the selection from the strip's latest scroll on every frame.
                left_scrolled: None,
            });
        self.arrival = Some(Arrival {
            from,
            carrier,
            first_frame: true,
        });
    }

    /// Aims the arrival's selection and closing gap from the latest layout, and ends the arrival
    /// once neither remains.
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
        let estimate = |position: usize| {
            arrival
                .from
                .slots
                .get(position)
                .map(|(start, size)| (*start - arrival.from.scrolled, *size))
                .unwrap_or((arrival.from.end, px(0.0)))
        };
        let aim = self
            .flight
            .as_ref()
            .map(|flight| flight.id)
            .zip(arrival.from.selection)
            .and_then(|(id, from)| {
                let position = ids.iter().position(|candidate| *candidate == id)?;
                let to = if laid_out {
                    settled(id)? + self.scrolled
                } else {
                    estimate(position).0
                } + self.selected_inset;
                Some(from - to)
            });
        let close = arrival
            .carrier
            .filter(|carrier| self.trailing_gaps.contains_key(carrier))
            .and_then(|carrier| {
                let end = if laid_out {
                    let (_, size) = self.laid_out.get(&carrier)?;
                    settled(carrier)? + *size + self.scrolled
                } else {
                    let (start, size) = estimate(ids.len() - 1);
                    start + size
                };
                Some((carrier, (arrival.from.end - end).max(px(0.0))))
            });
        if let (Some(flight), Some(from)) = (self.flight.as_mut(), aim) {
            flight.from = from;
        }
        if let Some((carrier, extent)) = close
            && let Some(gap) = self.trailing_gaps.get_mut(&carrier)
        {
            gap.extent = extent;
        }
        if aim.is_none() && close.is_none() {
            self.arrival = None;
        } else if let Some(arrival) = self.arrival.as_mut() {
            arrival.first_frame = false;
        }
    }

    /// Moves the selection to `selected`, a row of an item and how far that row stands from the
    /// item's start along the strip this frame, from wherever the selection was drawn, when the
    /// row differs from the last refresh. Call it on every render, after [`Self::refresh`].
    pub(crate) fn follow_row_selection(
        &mut self,
        selected: Option<(Id, Row, Pixels)>,
        window: &mut Window,
        cx: &App,
    ) {
        let now = now(cx);
        if self.lift.is_some() {
            self.flight = None;
        }
        let inset = selected.map_or(px(0.0), |(_, _, inset)| inset);
        let selected = selected.map(|(id, row, _)| (id, row));
        if selected != self.selected {
            self.arrival = None;
            let previous = std::mem::replace(&mut self.selected, selected);
            let motion = presence_motion(cx);
            // A selection that leaves a removed item moves with the gap it leaves, so it holds
            // still where that gap closes under it.
            let removed = previous.is_some_and(|(previous, _)| {
                self.known
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&previous))
            });
            // A selection that reaches an item growing from the strip's end moves with its growth,
            // which the strip scrolls along, so it holds still at that end.
            let growing_from_end =
                selected.is_some_and(|(selected, _)| self.growing_from_end.contains(&selected));
            let duration = if removed || growing_from_end {
                PRESENCE_DURATION
            } else {
                SELECTION_DURATION
            };
            let drawn = self.drawn_selection.take();
            let from =
                previous.and_then(|previous| drawn.or_else(|| self.selection_start(previous, now)));
            let to = selected.and_then(|(selected, _)| {
                Some(self.settled_start(selected, now)? + self.slid(selected, now) + inset)
            });
            self.flight = from
                .zip(to)
                .zip(selected)
                .filter(|_| self.lift.is_none() && motion != spaceterm_ui::ControlMotion::Reduced)
                .map(|((from, to), (id, row))| Flight {
                    id,
                    row,
                    from: from - to,
                    ease: Ease::new(duration, now, motion),
                    // A strip whose items grow in scrolls along with their growth, which the
                    // selection follows.
                    left_scrolled: self.entering.is_empty().then_some(self.scrolled),
                });
        }
        self.selected_inset = inset;
        self.drawn_selection = None;
        self.begin_arrival(now, cx);
        self.follow_arrival(now);
        if self.flight.as_ref().is_some_and(|flight| {
            flight.ease.finished(now) && self.displacement(flight.id, now) == px(0.0)
        }) {
            self.flight = None;
        }
        if self.flight.is_some() {
            window.request_animation_frame();
        }
    }

    /// Where the selection drawn at the row `row` of the item `id`, the last selected row, stands
    /// along the strip at `now`, or `None` when the last frame did not draw the item. A selection
    /// in flight stands off the row's settled start by what remains of its flight; otherwise it
    /// stands at the row in the item's face, which items growing in and gaps closing still
    /// displace.
    fn selection_start(&self, (id, row): (Id, Row), now: Instant) -> Option<Pixels> {
        if !self.laid_out.contains_key(&id) {
            return None;
        }
        let off_settled = self
            .flight
            .as_ref()
            .filter(|flight| flight.id == id && flight.row == row)
            .map_or_else(
                || self.displacement(id, now) + self.lag(id, now),
                |flight| flight.from * (1.0 - flight.ease.at(now)),
            );
        Some(self.settled_start(id, now)? + self.slid(id, now) + self.selected_inset + off_settled)
    }

    /// How far the item `id` slides along the strip at `now` toward where a drop placed it.
    fn slid(&self, id: Id, now: Instant) -> Pixels {
        self.slides
            .get(&id)
            .map_or(px(0.0), |slide| slide.at(now).along(self.axis))
    }

    /// How far the face of the item `id` starts short of its slot's start at `now`, while it grows
    /// from its slot's far end and the slot still holds back part of it.
    fn lag(&self, id: Id, now: Instant) -> Pixels {
        if !self.growing_from_end.contains(&id) {
            return px(0.0);
        }
        self.entering
            .get(&id)
            .filter(|(ease, _)| !ease.finished(now))
            .and_then(|(ease, natural)| natural.map(|natural| natural * (ease.at(now) - 1.0)))
            .unwrap_or(px(0.0))
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
        // A strip that scrolls to reveal the newly selected item does so on the frame the
        // selection leaves, so the selection leaves from where it was drawn before that scroll.
        // Later scrolling carries the selection along.
        if let Some(flight) = self.flight.as_mut()
            && let Some(left_scrolled) = flight.left_scrolled.take()
        {
            flight.from += left_scrolled - self.scrolled;
        }
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
        self.visible_end = visible_end - self.scrolled;
    }

    /// The size of the new item at `position` among the strip's `ids`, estimated from a neighbor,
    /// when it reaches past the visible end of an overflowing strip once grown, judged from the
    /// last frame's layout. A strip that fits its items ends at its last item, and grows with a
    /// new one.
    fn reaches_past_visible_end(&self, ids: &[Id], position: usize) -> Option<Pixels> {
        if !self.overflowing {
            return None;
        }
        let laid_out = |candidate: &Id| self.laid_out.get(candidate).copied();
        let next = ids[position + 1..].iter().find_map(laid_out);
        let previous = ids[..position].iter().rev().find_map(laid_out);
        let start = next
            .map(|(start, _)| start)
            .or(previous.map(|(start, size)| start + size));
        // A neighbor that is still growing in has its full size ahead of it.
        let size = |candidate: &Id| {
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
        // An item that returns while its old place still closes draws only where it returns.
        for gap in self
            .leading_gaps
            .values_mut()
            .chain(self.trailing_gaps.values_mut())
        {
            gap.leaving.retain(|leaving| !ids.contains(&leaving.id));
        }
        for (position, id) in ids.iter().enumerate() {
            if known.contains(id) {
                continue;
            }
            // An item that grows from the strip's end starts its face at the estimated size, so
            // the selection inside it lines up before it is measured.
            let estimate = self.reaches_past_visible_end(ids, position);
            if estimate.is_some() {
                self.growing_from_end.insert(*id);
            }
            self.entering
                .insert(*id, (Ease::new(PRESENCE_DURATION, now, motion), estimate));
        }
        let bounds = (0..known.len())
            .map(|position| items.bounds_for_item(position))
            .collect::<Option<Vec<_>>>()
            .filter(|bounds| bounds.len() == known.len());
        // How many items this change already put before each carrier's older leading gap.
        let mut placed = HashMap::new();
        for (position, id) in known.iter().enumerate() {
            if ids.contains(id) {
                continue;
            }
            self.entering.remove(id);
            // The slot's bounds hold its own closing gaps, and the items that left from them,
            // which keep what they show of themselves as their place joins this one.
            let lead = self.leading_gaps.remove(id);
            let trail = self.trailing_gaps.remove(id);
            let Some(bounds) = bounds.as_ref() else {
                continue;
            };
            let extent = bounds[position].size.along(self.axis);
            let painted = |gap: &Option<Gap<Id>>| {
                self.refreshed_at
                    .zip(gap.as_ref())
                    .map_or(px(0.0), |(at, gap)| gap.at(at))
            };
            let face = (extent - painted(&lead) - painted(&trail)).max(px(0.0));
            let shown = |gap: Option<Gap<Id>>| {
                gap.map_or_else(Vec::new, |gap| {
                    let open = gap.at(now);
                    shown_leaving(gap.leaving, open)
                })
            };
            let leaving = shown(lead)
                .into_iter()
                .chain([Leaving {
                    id: *id,
                    extent: face,
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
            let open = gap.as_ref().map_or(px(0.0), |gap| gap.at(now));
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
            gaps.insert(
                carrier,
                Gap {
                    extent: open + extent,
                    ease: Ease::new(PRESENCE_DURATION, now, motion),
                    leaving: older,
                },
            );
        }
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
            .flat_map(|gap| gap.leaving.iter().map(|leaving| leaving.id))
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
        self.leading_gaps.retain(|_, gap| !gap.ease.finished(now));
        self.trailing_gaps.retain(|_, gap| !gap.ease.finished(now));
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
    pub(crate) fn take_place(&mut self, items: &ScrollHandle, ids: &[Id], cx: &App) {
        if self.lift.is_some() {
            return;
        }
        let from = self.arriving.take().or_else(|| self.shape(items, cx));
        *self = Self {
            known: Some(ids.to_vec()),
            ..Self::new(self.axis)
        };
        if let Some(from) = from {
            self.arrive(from, cx);
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
        let now = now(cx);
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
            selection_offset: self
                .flight
                .as_ref()
                .filter(|flight| flight.id == id)
                .map_or_else(Point::default, |flight| self.flight_offset(flight, now)),
            extent: entering
                .map(|(ease, natural)| natural.map_or(px(0.0), |natural| natural * ease.at(now))),
            grows_from_end,
            leading_gap: self
                .leading_gaps
                .get(&id)
                .map_or(px(0.0), |gap| gap.at(now)),
            trailing_gap: self
                .trailing_gaps
                .get(&id)
                .map_or(px(0.0), |gap| gap.at(now)),
        }
    }

    /// Records where the selection was drawn along the strip in the last frame, as its owner
    /// measured the painted selection, so a selection that leaves on the next refresh starts
    /// exactly there. Call it before [`Self::follow_row_selection`].
    pub(crate) fn drew_selection_at(&mut self, start: Pixels) {
        self.drawn_selection = Some(start);
    }

    /// How much of the selection's move to the row `row` of the item `id` remains, from 1 as it
    /// leaves to 0 once it arrives.
    pub(crate) fn row_selection_remaining(&self, id: Id, row: Row, cx: &App) -> f32 {
        self.flight
            .as_ref()
            .filter(|flight| flight.id == id && flight.row == row && self.lift.is_none())
            .map_or(0.0, |flight| 1.0 - flight.ease.at(now(cx)))
    }

    /// How far the selection drawn at the row `row` of the item `id` stands from that row while it
    /// moves there.
    pub(crate) fn row_selection_offset(&self, id: Id, row: Row, cx: &App) -> Point<Pixels> {
        self.flight
            .as_ref()
            .filter(|flight| flight.id == id && flight.row == row)
            .map_or_else(Point::default, |flight| self.flight_offset(flight, now(cx)))
    }

    /// How far the selection in `flight` stands from its row at `now`.
    fn flight_offset(&self, flight: &Flight<Id, Row>, now: Instant) -> Point<Pixels> {
        if self.lift.is_some() {
            return Point::default();
        }
        let offset = flight.from * (1.0 - flight.ease.at(now))
            - self.displacement(flight.id, now)
            - self.lag(flight.id, now);
        Point::default().apply_along(self.axis, |_| offset)
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
        let landing = current.zip(copy).map(|(current, copy)| Landing {
            position: landing_position(
                &spans(axis, &slots),
                current,
                f32::from(origin.along(axis) + copy.size.along(axis) / 2.0),
            ),
            len: ids.len(),
        });
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
            self.track(items, ids, pointer, cx);
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
        self.track(items, ids, pointer, cx);
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

/// A progress from 0 to 1 that starts at `since`, fast at first and slowing to a stop.
#[derive(Clone, Copy, Debug)]
pub(super) struct Ease {
    since: Instant,
    duration: Duration,
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
        decay(self.progress(now))
    }
}

/// The space removed items left in the strip, closing from `extent` to nothing.
#[derive(Clone, Debug)]
struct Gap<Id> {
    extent: Pixels,
    ease: Ease,
    /// The removed items whose place the gap holds, in presentation order, which a strip may
    /// draw in it while it closes.
    leaving: Vec<Leaving<Id>>,
}

/// A removed item a closing gap holds, and how much of it shows from its start: all of it when
/// it left, or what showed when the gap that held it joined another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Leaving<Id> {
    pub(crate) id: Id,
    pub(crate) extent: Pixels,
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

impl<Id> Gap<Id> {
    fn at(&self, now: Instant) -> Pixels {
        self.extent * (1.0 - self.ease.at(now))
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
            .max(SELECTION_DURATION),
    );
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

/// The bounds a scroll container painted its items at, in presentation order.
fn painted_item_bounds(items: &ScrollHandle) -> Vec<Bounds<Pixels>> {
    let offset = items.offset();
    (0..items.children_count())
        .filter_map(|position| items.bounds_for_item(position))
        .map(|bounds| Bounds::new(bounds.origin + offset, bounds.size))
        .collect()
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

/// The face's origin in the window, while the owner places it.
type PreviewOrigin = dyn Fn(&App) -> Option<Point<Pixels>>;

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
        origin: impl Fn(&App) -> Option<Point<Pixels>> + 'static,
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
                let root = window.mouse_position() - placement.cursor_offset;
                (placement.origin)(cx).map(|origin| origin - root)
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

    #[test]
    fn undecay_should_find_the_progress_that_decay_reaches() {
        assert_eq!((undecay(0.0), undecay(1.0)), (0.0, 1.0));
        for progress in [0.1, 0.25, 0.5, 0.9] {
            assert!((undecay(decay(progress)) - progress).abs() < 1e-4);
        }
    }

    #[test]
    fn decay_should_start_at_full_speed_and_land_exactly() {
        assert_eq!((decay(0.0), decay(1.0)), (0.0, 1.0));
        assert!(decay(0.1) > 0.4, "the curve moves fastest at the start");
        assert!(decay(0.5) < decay(0.75) && decay(0.75) < 1.0);
    }
}
