//! Pointer drag and drop shared by the Tab bar, the Workspace sidebar, and Pane Layouts.
//! A [`DragSession`] ties owner state to the one GPUI drag it started, so an unseen release cannot
//! strand it.

use gpui::prelude::*;
use std::collections::HashMap;
use std::hash::Hash;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    Along as _, AnyElement, App, Axis, Bounds, BoxShadow, Context, DispatchPhase, KeystrokeEvent,
    MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, ScrollHandle, Size, Subscription,
    Window, WindowId, canvas, div, point, px,
};

use super::appearance::gpui_color;
use super::selection_chip::SelectionChip;

/// How long the other items take to slide aside as the landing slot moves.
const SHIFT_DURATION: Duration = Duration::from_millis(200);
/// How long a released item takes to settle into its slot, and the others to follow it.
const SETTLE_DURATION: Duration = Duration::from_millis(260);
/// How far past a strip the pointer may stray before the lifted item leaves it.
pub(crate) const TEAR_OFF_DISTANCE: f32 = 48.0;
/// How opaque a lifted item is, so the items it passes over show through a little.
pub(crate) const LIFTED_OPACITY: f32 = 0.95;

/// The drag state of one scrolling strip of items laid out along one axis.
///
/// While an item is lifted, its lifted copy follows the pointer and the other items slide aside to
/// open a gap where a release would land it. The item's empty place moves into that gap, and back
/// to rest once the pointer leaves the strip. A release settles the item into its place; a
/// cancellation returns everything to rest.
pub(crate) struct ReorderableStrip<Id> {
    axis: Axis,
    lift: Option<Lift<Id>>,
    /// Each item's displacement from its laid-out place, as it eases toward its target.
    slides: HashMap<Id, Slide>,
    /// The released item while it settles, which still shows its lift.
    settling: Option<Id>,
}

/// The item a strip's drag carries, its session, and where a release would land it.
struct Lift<Id> {
    id: Id,
    session: DragSession,
    /// Where the press held the item, from the item's origin.
    grab: Point<Pixels>,
    /// Where the lifted copy's origin stands in the window.
    origin: Option<Point<Pixels>>,
    landing: Option<Landing>,
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
    /// Whether the item is lifted, so its place stays empty.
    pub(crate) lifted: bool,
    /// How much of its lift a released item still shows, from 1 at release to 0 at rest.
    pub(crate) lift: f32,
}

impl<Id: Copy + Eq + Hash> ReorderableStrip<Id> {
    pub(crate) fn new(axis: Axis) -> Self {
        Self {
            axis,
            lift: None,
            slides: HashMap::new(),
            settling: None,
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
        let rest = ids
            .iter()
            .position(|candidate| *candidate == id)
            .and_then(|position| painted_item_bounds(items).get(position).copied());
        let grab = rest.map_or_else(Point::default, |rest| grab_point(window, cx) - rest.origin);
        self.lift = Some(Lift {
            id,
            session,
            grab,
            origin: None,
            landing: None,
        });
    }

    /// Prepares a frame. Ends a lift whose drag GPUI no longer carries, so a drag released while
    /// the strip was hidden leaves nothing behind, and keeps frames coming while items slide.
    /// Call it on every render.
    pub(crate) fn refresh(&mut self, window: &mut Window, cx: &App) {
        if self
            .lift
            .as_ref()
            .is_some_and(|lift| !lift.session.is_active(cx))
        {
            self.lift = None;
            self.slides.clear();
        }
        let now = now(cx);
        self.slides
            .retain(|_, slide| !slide.finished(now) || slide.to != Point::default());
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
        ItemMotion {
            offset: slide.map_or_else(Point::default, |slide| slide.at(now)),
            lifted: self.dragged() == Some(id),
            lift: slide
                .filter(|_| self.settling == Some(id))
                .map_or(0.0, |slide| 1.0 - ease_out(slide.progress(now))),
        }
    }

    /// Follows the pointer with the lifted copy and the slot a release would land the item in.
    /// Returns whether either changed.
    pub(crate) fn track(
        &mut self,
        items: &ScrollHandle,
        ids: &[Id],
        pointer: Point<Pixels>,
        cx: &App,
    ) -> bool {
        let axis = self.axis;
        let Some(lift) = self.lift.as_mut() else {
            return false;
        };
        let bounds = painted_item_bounds(items);
        let origin = pointer - lift.grab;
        let landing = ids
            .iter()
            .position(|candidate| *candidate == lift.id)
            .filter(|_| bounds.len() == ids.len())
            .filter(|_| {
                items
                    .bounds()
                    .dilate(px(TEAR_OFF_DISTANCE))
                    .contains(&pointer)
            })
            .map(|current| Landing {
                position: landing_position(
                    &spans(axis, &bounds),
                    current,
                    f32::from(origin.along(axis)),
                ),
                len: ids.len(),
            });
        let moved = lift.origin != Some(origin);
        lift.origin = Some(origin);
        let relanded = lift.landing != landing;
        lift.landing = landing;
        if relanded {
            self.open_gap(ids, &bounds, cx);
        }
        moved || relanded
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
        let bounds = painted_item_bounds(items);
        let Some(current) = ids
            .iter()
            .position(|candidate| *candidate == lift.id)
            .filter(|_| bounds.len() == ids.len())
        else {
            self.slides.clear();
            return None;
        };
        let landing = landing.filter(|position| *position != current);
        let now = now(cx);
        let motion = super::appearance_runtime::control_motion(cx);
        let spans = spans(self.axis, &bounds);
        match landing {
            // Each item keeps easing toward the gap it was opening, now its laid-out place.
            Some(position) => {
                for (id, shift) in ids.iter().zip(gap_shifts(&spans, current, position)) {
                    let shift = Point::default().apply_along(self.axis, |_| px(shift));
                    if let Some(slide) = self.slides.get_mut(id) {
                        *slide = slide.rebased(shift);
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
        let place = bounds[current].origin.apply_along(self.axis, |_| px(start));
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
        let eased = spring(self.progress(now));
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

/// Fast at the start and gentle at the end, as CSS `cubic-bezier(0.2, 0.8, 0.2, 1)`.
fn ease_out(progress: f32) -> f32 {
    cubic_bezier((0.2, 0.8), (0.2, 1.0), progress)
}

/// Fast at the start, past the target, and back onto it, as CSS
/// `cubic-bezier(0.34, 1.4, 0.64, 1)`.
fn spring(progress: f32) -> f32 {
    cubic_bezier((0.34, 1.4), (0.64, 1.0), progress)
}

/// The CSS timing function with control points `first` and `second`, at `progress`.
fn cubic_bezier(first: (f32, f32), second: (f32, f32), progress: f32) -> f32 {
    if progress <= 0.0 {
        return 0.0;
    }
    if progress >= 1.0 {
        return 1.0;
    }
    let bezier = |t: f32, first: f32, second: f32| {
        let rest = 1.0 - t;
        3.0 * rest * rest * t * first + 3.0 * rest * t * t * second + t * t * t
    };
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    let mut t = progress;
    for _ in 0..24 {
        let x = bezier(t, first.0, second.0);
        if (x - progress).abs() < 1e-5 {
            break;
        }
        if x < progress {
            low = t;
        } else {
            high = t;
        }
        t = (low + high) / 2.0;
    }
    bezier(t, first.1, second.1)
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

/// The chip that marks where a release would land a lifted item: `chip` as it looks under the
/// pointer, without the item's content.
pub(crate) fn drop_placeholder(chip: SelectionChip, selector: String) -> AnyElement {
    chip.render(selector, 1.0)
}

/// How opaque an item is that shows `lift` of its lift.
pub(crate) fn lifted_opacity(lift: f32) -> f32 {
    1.0 - (1.0 - LIFTED_OPACITY) * lift.clamp(0.0, 1.0)
}

/// Finishes every strip slide in progress, since test windows have no frame loop.
#[cfg(test)]
pub(crate) fn finish_slides(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(SHIFT_DURATION.max(SETTLE_DURATION));
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
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
    fn ease_out_should_start_fast_and_land_exactly() {
        assert_eq!((ease_out(0.0), ease_out(1.0)), (0.0, 1.0));
        assert!(ease_out(0.25) > 0.5, "the curve front-loads its motion");
        assert!(ease_out(0.5) < ease_out(0.75));
    }

    #[test]
    fn spring_should_overshoot_its_target_and_land_exactly() {
        assert_eq!((spring(0.0), spring(1.0)), (0.0, 1.0));
        assert!(
            (1..10).any(|tenth| spring(tenth as f32 / 10.0) > 1.0),
            "the curve passes its target before it lands"
        );
    }
}
