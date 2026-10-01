//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//!
//! A reorderable strip moves its dragged item live: the item takes a neighbour's place as soon as
//! the pointer crosses that neighbour's midpoint, so the gap it leaves follows the pointer the way
//! an AppKit tab bar and source list do. The lifted item follows the pointer as a [`DragPreview`].
//!
//! GPUI owns each drag's lifetime: it ends a drag on any release. A [`DragSession`] ties an owner's
//! state to the one drag it started, so the owner keeps that state only while GPUI still carries
//! that drag, and only that owner answers Escape. A release the owner never saw, such as one while
//! it was hidden, cannot strand its state or let it act on a later drag.

use gpui::prelude::*;
use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, Context, DispatchPhase, MouseUpEvent, Pixels, Point,
    ScrollHandle, SharedString, Size, Subscription, Window, canvas, div,
};

use super::appearance::gpui_color;
use super::chrome_typography::{ChromeTextStyleExt as _, TextRole};

/// The leading and trailing air inside a lifted item that has no shape of its own to copy.
const PREVIEW_HORIZONTAL_PADDING: f32 = 10.0;

/// The live reorder state of one scrolling strip of items laid out along one axis.
///
/// The strip reads its items' painted bounds from the scroll container that lays them out, so
/// the geometry follows scrolling. Items are equal in size along the axis, so a move between
/// frames leaves every slot where the last frame painted it.
pub(crate) struct ReorderableStrip<Id> {
    axis: Axis,
    lift: Option<Lift<Id>>,
}

/// The item a strip's drag carries, the order the strip had when it was lifted, and its session.
struct Lift<Id> {
    id: Id,
    origin: Vec<Id>,
    session: DragSession,
}

impl<Id: Copy + Eq> ReorderableStrip<Id> {
    pub(crate) const fn new(axis: Axis) -> Self {
        Self { axis, lift: None }
    }

    /// Lifts the item `id` from the strip whose items are in `order`.
    pub(crate) fn begin(&mut self, id: Id, order: Vec<Id>, session: DragSession) {
        self.lift = Some(Lift {
            id,
            origin: order,
            session,
        });
    }

    /// Ends a lift whose drag GPUI no longer carries.
    ///
    /// Call it on every render, so a drag released anywhere, including while the strip was
    /// hidden, leaves no lifted item behind.
    pub(crate) fn end_released(&mut self, cx: &App) {
        if self
            .lift
            .as_ref()
            .is_some_and(|lift| !lift.session.is_active(cx))
        {
            self.lift = None;
        }
    }

    /// Cancels the drag, returning the dragged item and the position among `current` that returns
    /// it between the same surviving neighbours it was lifted from.
    pub(crate) fn cancel(&mut self, current: &[Id]) -> Option<(Id, usize)> {
        let lift = self.lift.take()?;
        Some((lift.id, restored_position(&lift.origin, lift.id, current)))
    }

    pub(crate) fn dragged(&self) -> Option<Id> {
        self.lift.as_ref().map(|lift| lift.id)
    }

    /// Returns the position the dragged item takes for the pointer, when it differs from
    /// `current`, the item's position among the `items` the strip's scroll container laid out.
    ///
    /// A container that laid out a different number of items describes a stale frame and moves
    /// nothing.
    pub(crate) fn reorder(
        &self,
        items: &ScrollHandle,
        current: usize,
        len: usize,
        pointer: Point<Pixels>,
    ) -> Option<usize> {
        let item_bounds = painted_item_bounds(items);
        if self.lift.is_none() || item_bounds.len() != len || current >= len {
            return None;
        }
        let spans = item_bounds
            .iter()
            .map(|bounds| {
                let start = bounds.origin.along(self.axis);
                (
                    f32::from(start),
                    f32::from(start + bounds.size.along(self.axis)),
                )
            })
            .collect::<Vec<_>>();
        let position = reorder_position(&spans, current, f32::from(pointer.along(self.axis)));
        (position != current).then_some(position)
    }
}

/// The position among `current` that puts `dragged` back after the items that preceded it in
/// `origin` and still remain.
///
/// Only the dragged item moves during a drag, so the remaining items keep their relative order and
/// the surviving predecessors lead the strip once the dragged item is set aside. Items closed during
/// the drag drop out of the count instead of shifting the item past its neighbours.
fn restored_position<Id: Copy + Eq>(origin: &[Id], dragged: Id, current: &[Id]) -> usize {
    let predecessors = origin
        .iter()
        .take_while(|id| **id != dragged)
        .filter(|id| current.contains(id))
        .count();
    predecessors.min(current.len().saturating_sub(1))
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

/// The position the item at `dragged` takes when the pointer is at `pointer` along the strip.
///
/// `spans` are the start and end of every item in presentation order. The item passes each
/// neighbour whose midpoint the pointer has crossed, so items of different sizes neither skip nor
/// oscillate.
fn reorder_position(spans: &[(f32, f32)], dragged: usize, pointer: f32) -> usize {
    let midpoint = |(start, end): (f32, f32)| (start + end) / 2.0;
    let later = spans[dragged + 1..]
        .iter()
        .take_while(|span| pointer > midpoint(**span))
        .count();
    if later > 0 {
        return dragged + later;
    }
    let earlier = spans[..dragged]
        .iter()
        .rev()
        .take_while(|span| pointer < midpoint(**span))
        .count();
    dragged - earlier
}

/// The lifted copy of a dragged item that follows the pointer.
///
/// It rests on the anchored floating material, so a lifted Tab, Workspace, or Pane reads as one
/// family of raised shapes, and it takes the size of the item it lifts when that size is known.
pub(crate) struct DragPreview {
    label: SharedString,
    size: Option<Size<Pixels>>,
}

impl DragPreview {
    pub(crate) fn new(label: impl Into<SharedString>, size: Option<Size<Pixels>>) -> Self {
        Self {
            label: label.into(),
            size,
        }
    }
}

impl Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = super::appearance::chrome(cx);
        let shell =
            spaceterm_ui::floating_surface_theme(cx).shell(spaceterm_ui::FloatingRole::Popover);
        let label = self.label.clone();
        let frame = div()
            .debug_selector(|| "drag-preview".to_owned())
            .h(self
                .size
                .map_or(appearance.top_height(), |size| size.height))
            .when_some(self.size, |frame, size| frame.w(size.width))
            .px(appearance.spacing(PREVIEW_HORIZONTAL_PADDING))
            .flex()
            .items_center()
            .chrome_text(appearance.typography.style(TextRole::Navigation))
            .text_color(gpui_color(appearance.colors.text))
            .child(div().min_w_0().truncate().child(label));
        shell.mount(frame)
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
    fn reorder_position_should_pass_each_neighbour_whose_midpoint_the_pointer_crossed() {
        let spans = strip(&[100.0, 100.0, 100.0, 100.0]);

        assert_eq!(
            [
                reorder_position(&spans, 1, 120.0),
                reorder_position(&spans, 1, 251.0),
                reorder_position(&spans, 1, 399.0),
                reorder_position(&spans, 1, 49.0),
                reorder_position(&spans, 2, -40.0),
            ],
            [1, 2, 3, 0, 0]
        );
    }

    #[test]
    fn restored_position_should_follow_surviving_neighbours() {
        // D was lifted from the end of A B C D and dragged to the front.
        let origin = ['A', 'B', 'C', 'D'];

        assert_eq!(
            [
                restored_position(&origin, 'D', &['D', 'A', 'B', 'C']),
                restored_position(&origin, 'D', &['D', 'A', 'C']),
                restored_position(&origin, 'B', &['B', 'A', 'C', 'D']),
                restored_position(&origin, 'B', &['B', 'C', 'D']),
            ],
            [3, 2, 1, 0]
        );
    }

    #[test]
    fn reorder_position_should_not_oscillate_across_items_of_different_sizes() {
        // A wide item dragged past the midpoint of a narrow neighbour takes its place. In the new
        // order the narrow neighbour lies behind the pointer, so the item stays put.
        let before = strip(&[200.0, 40.0]);
        let moved = reorder_position(&before, 0, 221.0);
        let after = strip(&[40.0, 200.0]);

        assert_eq!((moved, reorder_position(&after, 1, 221.0)), (1, 1));
    }
}
