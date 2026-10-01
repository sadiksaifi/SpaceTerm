//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//!
//! A reorderable strip moves its dragged item live: the item takes a neighbour's place as soon as
//! the pointer crosses that neighbour's midpoint, so the gap it leaves follows the pointer the way
//! an AppKit tab bar and source list do. The lifted item follows the pointer as a [`DragPreview`].

use gpui::prelude::*;
use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, Context, DispatchPhase, MouseButton, MouseUpEvent,
    Pixels, Point, ScrollHandle, SharedString, Size, Window, canvas, div,
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
    dragged: Option<Id>,
}

impl<Id: Copy + Eq> ReorderableStrip<Id> {
    pub(crate) const fn new(axis: Axis) -> Self {
        Self {
            axis,
            dragged: None,
        }
    }

    pub(crate) fn begin(&mut self, id: Id) {
        self.dragged = Some(id);
    }

    /// Ends the drag and reports whether one was in progress.
    pub(crate) fn finish(&mut self) -> bool {
        self.dragged.take().is_some()
    }

    pub(crate) const fn dragged(&self) -> Option<Id> {
        self.dragged
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
        if self.dragged.is_none() || item_bounds.len() != len || current >= len {
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

/// Calls `on_end` when the pointer that carries a drag is released anywhere in the window.
///
/// GPUI ends every drag on release, including one dropped outside every target, but tells only
/// the target. Mount this while a drag is in progress so its owner can clear the drag state.
pub(crate) fn drag_end_observer(
    on_end: impl Fn(&mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |_, _, window, _| {
            let on_end = on_end.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && event.button == MouseButton::Left {
                    on_end(window, cx);
                }
            });
        },
    )
    .absolute()
    .size_0()
    .into_any_element()
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
    fn reorder_position_should_not_oscillate_across_items_of_different_sizes() {
        // A wide item dragged past the midpoint of a narrow neighbour takes its place. In the new
        // order the narrow neighbour lies behind the pointer, so the item stays put.
        let before = strip(&[200.0, 40.0]);
        let moved = reorder_position(&before, 0, 221.0);
        let after = strip(&[40.0, 200.0]);

        assert_eq!((moved, reorder_position(&after, 1, 221.0)), (1, 1));
    }
}
