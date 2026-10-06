//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//! A [`DragSession`] ties owner state to the one GPUI drag it started, so an unseen release cannot
//! strand it.

use gpui::prelude::*;
use std::rc::Rc;

use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, Context, DispatchPhase, KeystrokeEvent, MouseButton,
    MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollHandle, Size, Subscription, Window,
    WindowId, canvas, div, px,
};

use super::appearance::gpui_color;

/// The thickness of the accent line that marks where a dragged strip item would land.
const INSERTION_MARKER_THICKNESS: f32 = 2.0;

/// The drop state of one scrolling strip of items laid out along one axis.
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

    /// Ends a lift whose drag GPUI no longer carries. Call it on every render, so a drag released
    /// while the strip was hidden leaves no marker behind.
    pub(crate) fn end_released(&mut self, cx: &App) {
        if self
            .lift
            .as_ref()
            .is_some_and(|lift| !lift.session.is_active(cx))
        {
            self.lift = None;
        }
    }

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

    /// Ends the drag on its release, returning the dragged item and its new position among the
    /// `len` items when it lands somewhere new.
    pub(crate) fn finish(&mut self, current: usize, len: usize, cx: &App) -> Option<(Id, usize)> {
        let lift = self.lift.take()?;
        if !lift.session.is_active(cx) {
            return None;
        }
        let insertion = lift.insertion.filter(|insertion| insertion.len == len)?;
        Some((lift.id, landing_position(insertion.slot, current)))
    }
}

/// The start and end of every item along the strip, when the pointer lies within the strip and the
/// container laid out `len` items.
fn strip_spans(
    items: &ScrollHandle,
    axis: Axis,
    len: usize,
    pointer: Point<Pixels>,
) -> Option<Vec<(f32, f32)>> {
    if !items.bounds().contains(&pointer) {
        return None;
    }
    let item_bounds = painted_item_bounds(items);
    (item_bounds.len() == len).then(|| {
        item_bounds
            .iter()
            .map(|bounds| {
                let start = bounds.origin.along(axis);
                (f32::from(start), f32::from(start + bounds.size.along(axis)))
            })
            .collect()
    })
}

/// The slot a release at `pointer` lands the item at `dragged` in, or `None` when that slot is
/// beside the item's own place.
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

pub(crate) fn painted_item_size(items: &ScrollHandle, position: usize) -> Option<Size<Pixels>> {
    items.bounds_for_item(position).map(|bounds| bounds.size)
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
}

/// Paints a dragged item's face for one frame.
type PreviewFace = dyn Fn(&mut Window, &mut App) -> AnyElement;

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
        }
    }

    /// A preview with nothing to paint, for a drag whose owner is gone.
    pub(crate) fn empty() -> Self {
        Self {
            lift: Point::default(),
            face: Box::new(|_, _| div().into_any_element()),
        }
    }
}

impl Render for DragPreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // GPUI lays the preview out as a root, which ignores its own offset, so the face moves.
        div().debug_selector(|| "drag-preview".to_owned()).child(
            div()
                .relative()
                .left(self.lift.x)
                .top(self.lift.y)
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
