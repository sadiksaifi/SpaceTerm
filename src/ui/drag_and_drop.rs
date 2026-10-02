//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//!
//! A dragged item stays in its place until release. A copy of it follows the pointer as a
//! [`DragPreview`], and the strip or Pane Layout marks in the accent color where a release would put
//! it. Releasing anywhere else, or pressing Escape, moves nothing.
//!
//! GPUI owns each drag's lifetime: it ends a drag on any release. A [`DragSession`] ties an owner's
//! state to the one drag it started, so the owner keeps that state only while GPUI still carries
//! that drag, and only that owner answers Escape. A release the owner never saw, such as one while
//! it was hidden, cannot strand its state or let it act on a later drag.

use gpui::prelude::*;
use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, Context, DispatchPhase, MouseUpEvent, Pixels, Point,
    ScrollHandle, Size, Subscription, Window, canvas, div, px,
};

use super::appearance::gpui_color;

/// The thickness of the accent line that marks where a dragged strip item would land.
const INSERTION_MARKER_THICKNESS: f32 = 2.0;

/// The drop state of one scrolling strip of items laid out along one axis.
///
/// The strip reads its items' painted bounds from the scroll container that lays them out, so
/// the geometry follows scrolling.
pub(crate) struct ReorderableStrip<Id> {
    axis: Axis,
    lift: Option<Lift<Id>>,
}

/// The item a strip's drag carries, its session, and the slot a release would put it in.
struct Lift<Id> {
    id: Id,
    session: DragSession,
    insertion: Option<Insertion>,
}

/// A slot between the items of a strip that held `len` items, numbered from zero before the first
/// item to `len` after the last.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Insertion {
    slot: usize,
    len: usize,
}

impl<Id: Copy + Eq> ReorderableStrip<Id> {
    pub(crate) const fn new(axis: Axis) -> Self {
        Self { axis, lift: None }
    }

    /// Lifts the item `id`. It keeps its place until a release lands it elsewhere.
    pub(crate) fn begin(&mut self, id: Id, session: DragSession) {
        self.lift = Some(Lift {
            id,
            session,
            insertion: None,
        });
    }

    /// Ends a lift whose drag GPUI no longer carries.
    ///
    /// Call it on every render, so a drag released anywhere, including while the strip was
    /// hidden, leaves no marker behind.
    pub(crate) fn end_released(&mut self, cx: &App) {
        if self
            .lift
            .as_ref()
            .is_some_and(|lift| !lift.session.is_active(cx))
        {
            self.lift = None;
        }
    }

    /// Drops the lift without moving anything.
    pub(crate) fn cancel(&mut self) {
        self.lift = None;
    }

    pub(crate) fn dragged(&self) -> Option<Id> {
        self.lift.as_ref().map(|lift| lift.id)
    }

    /// The slot the insertion marker shows, numbered among the `len` items the strip presents.
    pub(crate) fn insertion(&self, len: usize) -> Option<usize> {
        self.lift
            .as_ref()
            .and_then(|lift| lift.insertion)
            .filter(|insertion| insertion.len == len)
            .map(|insertion| insertion.slot)
    }

    /// Follows the pointer with the slot a release would land the dragged item in. Returns whether
    /// the slot changed.
    ///
    /// `current` is the dragged item's position among the `len` items the strip's scroll container
    /// laid out. A pointer outside the strip across its axis, a slot beside the item's own place,
    /// and a container that laid out a different number of items show no slot.
    pub(crate) fn track(
        &mut self,
        items: &ScrollHandle,
        current: usize,
        len: usize,
        pointer: Point<Pixels>,
    ) -> bool {
        let axis = self.axis;
        let Some(lift) = self.lift.as_mut() else {
            return false;
        };
        let insertion = strip_spans(items, axis, len, pointer)
            .and_then(|spans| insertion_slot(&spans, current, f32::from(pointer.along(axis))))
            .map(|slot| Insertion { slot, len });
        let changed = lift.insertion != insertion;
        lift.insertion = insertion;
        changed
    }

    /// Ends the drag on its release, returning the dragged item and the position it lands at among
    /// the `len` items the strip now presents, when it lands somewhere new.
    ///
    /// A drag GPUI no longer carries, such as one cancelled with Escape, lands nowhere.
    pub(crate) fn finish(&mut self, current: usize, len: usize, cx: &App) -> Option<(Id, usize)> {
        let lift = self.lift.take()?;
        if !lift.session.is_active(cx) {
            return None;
        }
        let insertion = lift.insertion.filter(|insertion| insertion.len == len)?;
        Some((lift.id, landing_position(insertion.slot, current)))
    }
}

/// The start and end of every item along the strip, when the pointer lies within the strip across
/// its axis and the container laid out `len` items.
fn strip_spans(
    items: &ScrollHandle,
    axis: Axis,
    len: usize,
    pointer: Point<Pixels>,
) -> Option<Vec<(f32, f32)>> {
    let strip = items.bounds();
    let across = axis.invert();
    let start = strip.origin.along(across);
    let end = start + strip.size.along(across);
    let pointer_across = pointer.along(across);
    if pointer_across < start || pointer_across >= end {
        return None;
    }
    let item_bounds = painted_item_bounds(items);
    (item_bounds.len() == len).then(|| {
        item_bounds
            .iter()
            .map(|bounds| {
                let start = bounds.origin.along(axis);
                (
                    f32::from(start),
                    f32::from(start + bounds.size.along(axis)),
                )
            })
            .collect()
    })
}

/// The slot a release at `pointer` lands the item at `dragged` in, or `None` when that slot is
/// beside the item's own place.
///
/// `spans` are the start and end of every item in presentation order. The slot follows every item
/// whose midpoint the pointer has passed.
fn insertion_slot(spans: &[(f32, f32)], dragged: usize, pointer: f32) -> Option<usize> {
    if dragged >= spans.len() {
        return None;
    }
    let slot = spans
        .iter()
        .take_while(|(start, end)| pointer > (start + end) / 2.0)
        .count();
    (slot != dragged && slot != dragged + 1).then_some(slot)
}

/// The position an item at `current` takes when it lands in `slot`.
///
/// The item leaves its own place first, so a slot after it is one position earlier.
fn landing_position(slot: usize, current: usize) -> usize {
    if slot > current { slot - 1 } else { slot }
}

/// Which side of its item a strip's insertion marker sits on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MarkerSide {
    Leading,
    Trailing,
}

/// The accent line that marks the slot a dragged strip item would land in.
///
/// It sits on the `side` of the item it is mounted in, across a strip laid out along `axis`, and
/// keeps `inset` from its two ends so it spans the item's shape rather than its hit target. A marker
/// between two items is centred on their shared edge; one at either end of the strip stays inside
/// it.
pub(crate) fn insertion_marker(
    axis: Axis,
    side: MarkerSide,
    at_strip_end: bool,
    inset: (Pixels, Pixels),
    selector: String,
    appearance: &super::appearance::ChromeAppearance,
) -> AnyElement {
    let thickness = px(INSERTION_MARKER_THICKNESS);
    let offset = if at_strip_end {
        px(0.0)
    } else {
        -thickness / 2.0
    };
    let marker = div()
        .debug_selector(move || selector.clone())
        .absolute()
        .rounded_full()
        .bg(gpui_color(appearance.colors.primary_background));
    match (axis, side) {
        (Axis::Horizontal, side) => {
            let marker = marker.top(inset.0).bottom(inset.1).w(thickness);
            match side {
                MarkerSide::Leading => marker.left(offset),
                MarkerSide::Trailing => marker.right(offset),
            }
        }
        (Axis::Vertical, side) => {
            let marker = marker.left(inset.0).right(inset.1).h(thickness);
            match side {
                MarkerSide::Leading => marker.top(offset),
                MarkerSide::Trailing => marker.bottom(offset),
            }
        }
    }
    .into_any_element()
}

/// The size the scroll container painted its item at `position`, used to lift it at that size.
pub(crate) fn painted_item_size(items: &ScrollHandle, position: usize) -> Option<Size<Pixels>> {
    items.bounds_for_item(position).map(|bounds| bounds.size)
}

/// The bounds a scroll container painted its items at, in presentation order.
///
/// The container records layout bounds before its scroll offset moves them.
fn painted_item_bounds(items: &ScrollHandle) -> Vec<Bounds<Pixels>> {
    let offset = items.offset();
    (0..items.children_count())
        .filter_map(|position| items.bounds_for_item(position))
        .map(|bounds| Bounds::new(bounds.origin + offset, bounds.size))
        .collect()
}

/// The lifted copy of a dragged item that follows the pointer.
///
/// A Tab or Workspace row paints its own face, so the copy looks exactly like the item it lifts. An
/// item without a compact face, such as a Pane, lifts a label on the anchored floating material.
pub(crate) struct DragPreview {
    face: Box<PreviewFace>,
}

/// Paints a dragged item's face for one frame.
type PreviewFace = dyn Fn(&mut Window, &mut App) -> AnyElement;

impl DragPreview {
    /// A preview that paints `face` at the pointer. The face must not claim the pointer: it lies
    /// over every drop target while the drag lasts.
    pub(crate) fn new(face: impl Fn(&mut Window, &mut App) -> AnyElement + 'static) -> Self {
        Self {
            face: Box::new(face),
        }
    }

    /// A preview with nothing to paint, for a drag whose owner is gone.
    pub(crate) fn empty() -> Self {
        Self::new(|_, _| div().into_any_element())
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "drag-preview".to_owned())
            .child((self.face)(window, cx))
    }
}

/// Calls `on_release` when a button is released anywhere in the window while a drag is active.
///
/// GPUI ends every drag on any release, including one dropped outside every target, but tells only
/// the target. Mount this while a drag is in progress so its owner can act on the release point
/// before GPUI ends the drag; the owner confirms the drag is its own with [`DragSession::is_active`].
pub(crate) fn drag_release_observer(
    on_release: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
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

/// The most recent drag a [`DragSession`] began in this application.
#[derive(Default)]
struct CurrentDrag {
    issued: u64,
    current: Option<u64>,
}

impl gpui::Global for CurrentDrag {}

/// One drag that one owner started.
///
/// Every Tab, Workspace, and Pane drag begins a session, so the session that began last names the
/// drag GPUI carries. While the session lasts, Escape cancels its drag before the key reaches any
/// focused element, so it never reaches a Terminal Session; a session whose drag has ended ignores
/// Escape.
pub(crate) struct DragSession {
    ticket: u64,
    _escape: Subscription,
}

impl DragSession {
    /// Begins a session for the drag GPUI is starting in `window`. Escape stops that drag and then
    /// calls `on_cancel` on the owner.
    pub(crate) fn begin<T: 'static>(
        window: &Window,
        cx: &mut Context<T>,
        on_cancel: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
    ) -> Self {
        let drags = cx.default_global::<CurrentDrag>();
        drags.issued += 1;
        let ticket = drags.issued;
        drags.current = Some(ticket);
        let window_id = window.window_handle().window_id();
        let owner = cx.weak_entity();
        let escape = cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle().window_id() != window_id
                || event.keystroke.key != "escape"
                || !carries(ticket, cx)
                || !cx.stop_active_drag(window)
            {
                return;
            }
            cx.stop_propagation();
            let _ = owner.update(cx, |owner, cx| on_cancel(owner, window, cx));
        });
        Self {
            ticket,
            _escape: escape,
        }
    }

    /// Whether GPUI still carries this session's drag.
    pub(crate) fn is_active(&self, cx: &App) -> bool {
        carries(self.ticket, cx)
    }
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
    fn insertion_slot_should_follow_each_item_whose_midpoint_the_pointer_passed() {
        let spans = strip(&[100.0, 100.0, 100.0, 100.0]);

        assert_eq!(
            [
                insertion_slot(&spans, 1, 40.0),
                insertion_slot(&spans, 1, 251.0),
                insertion_slot(&spans, 1, 399.0),
                insertion_slot(&spans, 1, 520.0),
                insertion_slot(&spans, 2, -40.0),
            ],
            [Some(0), Some(3), Some(4), Some(4), Some(0)]
        );
    }

    #[test]
    fn insertion_slot_should_show_nothing_beside_the_items_own_place() {
        let spans = strip(&[100.0, 100.0, 100.0]);

        assert_eq!(
            [
                insertion_slot(&spans, 1, 60.0),
                insertion_slot(&spans, 1, 150.0),
                insertion_slot(&spans, 1, 240.0),
            ],
            [None, None, None]
        );
    }

    #[test]
    fn landing_position_should_account_for_the_place_the_item_leaves() {
        // B is lifted from A B C D.
        assert_eq!(
            [
                landing_position(0, 1),
                landing_position(3, 1),
                landing_position(4, 1),
            ],
            [0, 2, 3]
        );
    }
}
