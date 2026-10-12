//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//! A [`DragSession`] ties owner state to the one GPUI drag it started, so an unseen release cannot
//! strand it.

use gpui::prelude::*;
use std::collections::HashMap;
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
const PRESENCE_DURATION: Duration = Duration::from_millis(440);
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
pub(crate) struct ReorderableStrip<Id> {
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
    /// The closing gap a removed item left before each item, inside that item's slot.
    leading_gaps: HashMap<Id, Gap>,
    /// The closing gap a removed last item left after the item that is now last, inside that
    /// item's slot.
    trailing_gaps: HashMap<Id, Gap>,
    /// When the strip last prepared a frame.
    refreshed_at: Option<Instant>,
    /// Where each item of the last frame started along the strip, and its size, in layout
    /// coordinates.
    laid_out: HashMap<Id, (Pixels, Pixels)>,
    /// The selected item at the last refresh.
    selected: Option<Id>,
    /// The selection moving to the selected item.
    flight: Option<Flight<Id>>,
    /// How far the strip was scrolled along its axis in the last frame.
    scrolled: Pixels,
    /// Whether the items of the last frame ran past the strip's visible end.
    overflowing: bool,
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

/// The selection moving from where it was drawn to the item `id`.
struct Flight<Id> {
    id: Id,
    /// Where the selection started along the strip, from where the item settles.
    from: Pixels,
    ease: Ease,
}

/// The item a strip's drag carries, its session, and where a release would land it.
struct Lift<Id> {
    id: Id,
    session: DragSession,
    /// Where the press held the item, from the item's origin.
    grab: Point<Pixels>,
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
    /// item grows in, and opens the closing gaps beside it. The item shows whole from the start
    /// and overlaps the items making room for it. `measure` receives the item's natural size
    /// while it grows, which needs `slot`'s only child to be the item. Both gaps stay inside the
    /// slot, so a scrolling strip counts them and closes them without jumping.
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
        slot.on_children_prepainted(move |bounds, _, cx| {
            if let Some(item) = bounds.first() {
                measure(item.size, cx);
            }
        })
    }
}

impl<Id: Copy + Eq + Hash> ReorderableStrip<Id> {
    pub(crate) fn new(axis: Axis) -> Self {
        Self {
            axis,
            lift: None,
            slides: HashMap::new(),
            settling: None,
            known: None,
            entering: HashMap::new(),
            grown: Vec::new(),
            leading_gaps: HashMap::new(),
            trailing_gaps: HashMap::new(),
            refreshed_at: None,
            laid_out: HashMap::new(),
            selected: None,
            flight: None,
            scrolled: px(0.0),
            overflowing: false,
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
        // The selection draws inside its item, which a drop may still be settling.
        let selection = self.selected.and_then(|id| {
            let position = ids.iter().position(|candidate| *candidate == id)?;
            let motion = self.motion(id, cx);
            Some(
                slots[position].0 + motion.offset.along(axis) + motion.selection_offset.along(axis),
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
    /// shows whole while its slot grows.
    fn face(&self, id: Id, slot: Bounds<Pixels>) -> Bounds<Pixels> {
        let item = self.trim(id, slot);
        match self.entering.get(&id).and_then(|(_, natural)| *natural) {
            Some(natural) => {
                Bounds::new(item.origin, item.size.apply_along(self.axis, |_| natural))
            }
            None => item,
        }
    }

    /// Trims `slot`, the bounds of the item `id`'s slot in the last frame, to the item itself.
    fn trim(&self, id: Id, slot: Bounds<Pixels>) -> Bounds<Pixels> {
        let painted = |gaps: &HashMap<Id, Gap>| {
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
        for id in ids.iter().skip(from.slots.len()).filter(|_| reshape) {
            let natural = self.laid_out.get(id).map(|(_, size)| *size);
            self.entering
                .entry(*id)
                .or_insert_with(|| (Ease::new(PRESENCE_DURATION, now, motion), natural));
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
                },
            );
        }
        self.flight = self
            .selected
            .filter(|_| from.selection.is_some())
            .map(|id| Flight {
                id,
                from: px(0.0),
                ease: Ease::new(SELECTION_DURATION, now, motion),
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
                };
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

    /// Moves the selection to the item `selected` from wherever it was drawn, when it differs
    /// from the last refresh. Call it on every render, after [`Self::refresh`].
    pub(crate) fn follow_selection(&mut self, selected: Option<Id>, window: &mut Window, cx: &App) {
        let now = now(cx);
        if self.lift.is_some() {
            self.flight = None;
        }
        if selected != self.selected {
            self.arrival = None;
            let previous = std::mem::replace(&mut self.selected, selected);
            let motion = presence_motion(cx);
            // A selection that leaves a removed item moves with the gap it leaves, so it holds
            // still where that gap closes under it.
            let removed = previous.is_some_and(|previous| {
                self.known
                    .as_ref()
                    .is_some_and(|ids| !ids.contains(&previous))
            });
            let duration = if removed {
                PRESENCE_DURATION
            } else {
                SELECTION_DURATION
            };
            let from = previous.and_then(|previous| self.selection_start(previous, now));
            let to = selected.and_then(|selected| self.settled_start(selected, now));
            self.flight = from
                .zip(to)
                .zip(selected)
                .filter(|_| self.lift.is_none() && motion != spaceterm_ui::ControlMotion::Reduced)
                .map(|((from, to), id)| Flight {
                    id,
                    from: from - to,
                    ease: Ease::new(duration, now, motion),
                });
        }
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

    /// Where the selection drawn at the item `id` started along the strip in the last frame.
    fn selection_start(&self, id: Id, now: Instant) -> Option<Pixels> {
        let (start, _) = *self.laid_out.get(&id)?;
        let flying = self
            .flight
            .as_ref()
            .filter(|flight| flight.id == id)
            .map_or(px(0.0), |flight| {
                flight.from * (1.0 - flight.ease.at(now)) - self.displacement(id, now)
            });
        Some(start + flying)
    }

    /// Where the item `id` starts along the strip once every item has grown in and every gap has
    /// closed. A new item settles where the item after it, or the end of the item before it,
    /// stood in the last frame.
    fn settled_start(&self, id: Id, now: Instant) -> Option<Pixels> {
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
            |gaps: &HashMap<Id, Gap>, id: &Id| gaps.get(id).map_or(px(0.0), |gap| gap.at(now));
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
        for id in ids.iter().filter(|id| !known.contains(id)) {
            self.entering
                .insert(*id, (Ease::new(PRESENCE_DURATION, now, motion), None));
        }
        let bounds = (0..known.len())
            .map(|position| items.bounds_for_item(position))
            .collect::<Option<Vec<_>>>()
            .filter(|bounds| bounds.len() == known.len());
        for (position, id) in known.iter().enumerate() {
            if ids.contains(id) {
                continue;
            }
            self.entering.remove(id);
            // The slot's bounds hold its own closing gaps.
            self.leading_gaps.remove(id);
            self.trailing_gaps.remove(id);
            let Some(bounds) = bounds.as_ref() else {
                continue;
            };
            let extent = bounds[position].size.along(self.axis);
            let next = known[position + 1..].iter().find(|id| ids.contains(id));
            let previous = known[..position].iter().rev().find(|id| ids.contains(id));
            let (gaps, carrier) = match (next, previous) {
                (Some(next), _) => (&mut self.leading_gaps, *next),
                (None, Some(previous)) => (&mut self.trailing_gaps, *previous),
                (None, None) => continue,
            };
            let open = gaps.get(&carrier).map_or(px(0.0), |gap| gap.at(now));
            gaps.insert(
                carrier,
                Gap {
                    extent: open + extent,
                    ease: Ease::new(PRESENCE_DURATION, now, motion),
                },
            );
        }
    }

    /// Lifts the item `id`, one of the strip's `ids` in presentation order. It keeps its place
    /// until a release lands it elsewhere.
    pub(crate) fn begin(
        &mut self,
        id: Id,
        session: DragSession,
        items: &ScrollHandle,
        ids: &[Id],
        window: &Window,
        cx: &App,
    ) {
        self.slides.clear();
        self.settling = None;
        self.arriving = None;
        self.arrival = None;
        let rest = ids
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|position| self.item_bounds(items, ids).get(position).copied());
        let grab = rest.map_or_else(Point::default, |rest| grab_point(window, cx) - rest.origin);
        self.lift = Some(Lift {
            id,
            session,
            grab,
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
        self.record_layout(items);
        self.follow_changes(items, ids, cx);
        self.autoscroll(items, ids, window, cx);
        let now = now(cx);
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
        ItemMotion {
            offset: slide.map_or_else(Point::default, |slide| slide.at(now)),
            lifted: self.dragged() == Some(id),
            lift: slide
                .filter(|_| self.settling == Some(id))
                .map_or(0.0, |slide| 1.0 - decay(slide.progress(now))),
            selection_offset: self
                .flight
                .as_ref()
                .filter(|flight| flight.id == id && self.lift.is_none())
                .map_or_else(Point::default, |flight| {
                    let offset =
                        flight.from * (1.0 - flight.ease.at(now)) - self.displacement(id, now);
                    match self.axis {
                        Axis::Horizontal => point(offset, px(0.0)),
                        Axis::Vertical => point(px(0.0), offset),
                    }
                }),
            extent: entering
                .map(|(ease, natural)| natural.map_or(px(0.0), |natural| natural * ease.at(now))),
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
        let origin = current.map_or(pointer - lift.grab, |current| {
            confine(axis, items.bounds(), faces[current], pointer - lift.grab)
        });
        let landing = current.map(|current| Landing {
            position: landing_position(
                &spans(axis, &slots),
                current,
                f32::from(origin.along(axis)),
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
    /// follows the items that pass under the lifted copy.
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

/// The position a lifted item whose leading edge stands at `start` lands in: past every other
/// item whose midpoint its center has passed. Reaching a midpoint passes the item.
fn landing_position(spans: &[(f32, f32)], dragged: usize, start: f32) -> usize {
    let (rest_start, rest_end) = spans[dragged];
    let center = start + (rest_end - rest_start) / 2.0;
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

fn presence_motion(cx: &App) -> spaceterm_ui::ControlMotion {
    #[cfg(test)]
    if !cx.has_global::<AnimatePresence>() {
        return spaceterm_ui::ControlMotion::Reduced;
    }
    super::appearance_runtime::control_motion(cx)
}

/// A progress from 0 to 1 that starts at `since`, fast at first and slowing to a stop.
#[derive(Clone, Copy, Debug)]
struct Ease {
    since: Instant,
    duration: Duration,
}

impl Ease {
    /// An ease that starts at `now`, or ends at once when motion is reduced.
    fn new(duration: Duration, now: Instant, motion: spaceterm_ui::ControlMotion) -> Self {
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

    fn finished(&self, now: Instant) -> bool {
        self.progress(now) >= 1.0
    }

    fn at(&self, now: Instant) -> f32 {
        decay(self.progress(now))
    }
}

/// The space a removed item left in the strip, closing from `extent` to nothing.
#[derive(Clone, Copy, Debug)]
struct Gap {
    extent: Pixels,
    ease: Ease,
}

impl Gap {
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
fn decay(progress: f32) -> f32 {
    /// How many time constants the motion runs, by when less than 1% of the distance remains.
    const TIME_CONSTANTS: f32 = 6.2;
    (1.0 - (-TIME_CONSTANTS * progress).exp()) / (1.0 - (-TIME_CONSTANTS).exp())
}

fn now(cx: &App) -> Instant {
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

        // B, lifted from A B C D, has its center 50 past its leading edge.
        assert_eq!(
            [
                landing_position(&spans, 1, -10.0),
                landing_position(&spans, 1, 40.0),
                landing_position(&spans, 1, 160.0),
                landing_position(&spans, 1, 201.0),
                landing_position(&spans, 1, 300.0),
                // A held at the leading end of the strip stays first.
                landing_position(&spans, 0, 0.0),
                // A copy carried past either end lands at that end.
                landing_position(&spans, 1, -500.0),
                landing_position(&spans, 1, 900.0),
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
    fn decay_should_start_at_full_speed_and_land_exactly() {
        assert_eq!((decay(0.0), decay(1.0)), (0.0, 1.0));
        assert!(decay(0.1) > 0.4, "the curve moves fastest at the start");
        assert!(decay(0.5) < decay(0.75) && decay(0.75) < 1.0);
    }
}
