//! Motion of a Tab's Panes as its Pane Layout changes.

use std::collections::BTreeMap;
use std::time::Instant;

use gpui::{App, Bounds, Edges, Pixels, point, px, size};

use super::drag_and_drop::{Ease, PRESENCE_DURATION, now, presence_motion};
use crate::domain::PaneId;

/// How far apart two edges may be and still count as touching, after layout rounding.
const EDGE_TOLERANCE: f32 = 0.5;

/// The Panes of one Pane Layout easing from where they were drawn to where a split, close, move,
/// zoom, or split reset lays them out.
///
/// Only a change announced by [`PaneMotion::expect_change`] or [`PaneMotion::expect_raise`] moves
/// Panes, so a window resize or a split resize drag lays them out at once and stops any motion
/// under way. After a layout change, a Pane that stays eases from where it was drawn. A new Pane
/// opens from its far edge as the Pane beside it makes room, and a removed Pane's place closes
/// toward its far edge as the Pane beside it takes the space. A raised Pane, such as one zooming,
/// moves alone above the others, which stay where they are and fade while it covers them.
#[derive(Default)]
pub(crate) struct PaneMotion {
    /// Where each Pane was laid out in the last frame, in Pane Layout coordinates.
    targets: BTreeMap<PaneId, Bounds<Pixels>>,
    /// Where each Pane was drawn in the last frame, in Pane Layout coordinates.
    drawn: BTreeMap<PaneId, Bounds<Pixels>>,
    /// Each moving Pane's bounds when its motion started.
    moves: BTreeMap<PaneId, Move>,
    /// The places of removed Panes as they close.
    closing: Vec<Closing>,
    /// The change the next frame's layout comes from, when an operation announced one.
    expected: Option<Change>,
    /// The change the last frame started moving Panes toward.
    started: Option<Change>,
    /// The Pane moving above the others.
    raised: Option<Raise>,
    /// The opacity of the Panes beneath the raised Pane in the last frame.
    beneath_opacity: f32,
}

/// A Pane moving above the others, whose surface lets the Panes beneath it show through.
#[derive(Clone, Copy, Debug)]
struct Raise {
    pane_id: PaneId,
    /// Whether the Pane grows over the Panes beneath it, which then fade out, rather than shrinking
    /// back into its place as they fade in.
    covers: bool,
    /// The opacity of the Panes beneath it when it started.
    beneath_from: f32,
}

/// An announced change of the Pane Layout.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Change {
    /// Every Pane moves to its new place.
    Layout,
    /// Only this Pane moves, above the others.
    Raise(PaneId),
}

#[derive(Clone, Copy, Debug)]
struct Move {
    from: Bounds<Pixels>,
    ease: Ease,
}

#[derive(Clone, Copy, Debug)]
struct Closing {
    pane_id: PaneId,
    from: Bounds<Pixels>,
    to: Bounds<Pixels>,
    ease: Ease,
}

/// Where one frame draws each Pane, and the places of removed Panes still closing.
#[derive(Debug)]
pub(crate) struct PaneFrames {
    frames: BTreeMap<PaneId, (Bounds<Pixels>, Bounds<Pixels>)>,
    /// Device pixels per Pane Layout pixel.
    scale_factor: f32,
    /// The closing places of removed Panes, in Pane Layout coordinates.
    pub(crate) closing: Vec<Bounds<Pixels>>,
    /// Whether any Pane or closing place is still moving.
    pub(crate) moving: bool,
    /// The Pane moving above the others, and how far it has come.
    pub(crate) raised: Option<(PaneId, f32)>,
    /// The opacity of the Panes beneath the raised Pane.
    pub(crate) beneath_opacity: f32,
}

impl Default for PaneFrames {
    fn default() -> Self {
        Self {
            frames: BTreeMap::new(),
            scale_factor: 1.0,
            closing: Vec::new(),
            moving: false,
            raised: None,
            beneath_opacity: 1.0,
        }
    }
}

impl PaneFrames {
    /// How far the Pane `pane_id` is drawn inside each edge of its laid-out place, in whole device
    /// pixels so that offsetting by one edge and back again by the sum of two keeps a size exact.
    /// Outward distances are negative.
    pub(crate) fn inset(&self, pane_id: PaneId) -> Edges<Pixels> {
        let Some((target, frame)) = self.frames.get(&pane_id) else {
            return Edges::default();
        };
        let device_pixels =
            |distance: Pixels| (distance * self.scale_factor).round() / self.scale_factor;
        Edges {
            top: device_pixels(frame.top() - target.top()),
            right: device_pixels(target.right() - frame.right()),
            bottom: device_pixels(target.bottom() - frame.bottom()),
            left: device_pixels(frame.left() - target.left()),
        }
    }
}

impl PaneMotion {
    /// Moves Panes from where they are drawn to the layout of the next frame, which an operation
    /// such as a split, close, move, or zoom changes.
    pub(crate) fn expect_change(&mut self) {
        self.expected = Some(Change::Layout);
    }

    /// Moves only `pane_id` from where it is drawn to its place in the next frame's layout, above
    /// the other Panes, which stay where they are. A zoom or unzoom changes the layout this way.
    pub(crate) fn expect_raise(&mut self, pane_id: PaneId) {
        self.expected = Some(Change::Raise(pane_id));
    }

    /// Starts the change the last frame started again, toward the next frame's layout. A change
    /// that also resizes the Pane Layout, such as an unzoom that raises its minimum size, is laid
    /// out at the measured size one frame late.
    pub(crate) fn restart_change(&mut self) {
        if let Some(change) = self.started {
            self.expected = Some(change);
        }
    }

    /// Where this frame draws each Pane laid out at `targets`, in Pane Layout coordinates, at
    /// `scale_factor` device pixels per Pane Layout pixel.
    pub(crate) fn present(
        &mut self,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
        scale_factor: f32,
        cx: &App,
    ) -> PaneFrames {
        PaneFrames {
            scale_factor,
            ..self.present_at(targets, now(cx), presence_motion(cx))
        }
    }

    fn present_at(
        &mut self,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) -> PaneFrames {
        // A layout not measured yet has nothing to move from or to.
        if targets.is_empty() {
            return PaneFrames::default();
        }
        self.started = self.expected.take().filter(|_| !self.drawn.is_empty());
        match self.started {
            Some(Change::Layout) => {
                self.raised = None;
                self.follow_change(targets, now, motion);
            }
            Some(Change::Raise(pane_id)) => self.follow_raise(pane_id, targets, now, motion),
            None if *targets != self.targets => {
                self.moves.clear();
                self.closing.clear();
            }
            None => {}
        }
        // Motion reduced during a change ends it.
        if motion == spaceterm_ui::ControlMotion::Reduced {
            self.moves.clear();
            self.closing.clear();
        }
        self.targets.clone_from(targets);
        self.moves
            .retain(|pane_id, motion| targets.contains_key(pane_id) && !motion.ease.finished(now));
        self.closing.retain(|closing| !closing.ease.finished(now));
        self.raised = self
            .raised
            .filter(|raise| self.moves.contains_key(&raise.pane_id));

        let frames: BTreeMap<_, _> = targets
            .iter()
            .map(|(pane_id, target)| {
                let frame = self.moves.get(pane_id).map_or(*target, |motion| {
                    between(motion.from, *target, motion.ease.at(now))
                });
                (*pane_id, (*target, frame))
            })
            .collect();
        self.drawn = frames
            .iter()
            .map(|(pane_id, (_, frame))| (*pane_id, *frame))
            .collect();
        PaneFrames {
            frames,
            closing: self
                .closing
                .iter()
                .map(|closing| between(closing.from, closing.to, closing.ease.at(now)))
                .collect(),
            moving: !self.moves.is_empty() || !self.closing.is_empty(),
            raised: self
                .raised
                .map(|raise| (raise.pane_id, self.moves[&raise.pane_id].ease.at(now))),
            beneath_opacity: self.beneath_opacity_at(now),
            ..PaneFrames::default()
        }
    }

    /// How opaque the Panes beneath the raised Pane draw at `now`. The raised Pane's surface lets
    /// them show through, so they fade out over the first half of its motion as it grows over
    /// them, and fade in over the second half as it shrinks back into its place.
    fn beneath_opacity_at(&mut self, now: Instant) -> f32 {
        let Some(raise) = self.raised else {
            return 1.0;
        };
        let progress = self.moves[&raise.pane_id].ease.at(now);
        self.beneath_opacity = if raise.covers {
            raise.beneath_from * (1.0 - 2.0 * progress).max(0.0)
        } else {
            raise.beneath_from + (1.0 - raise.beneath_from) * (2.0 * progress - 1.0).max(0.0)
        };
        self.beneath_opacity
    }

    /// Starts each Pane toward its place in `targets` from where it was last drawn.
    fn follow_change(
        &mut self,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) {
        let ease = Ease::new(PRESENCE_DURATION, now, motion);
        // A removed Pane that returns before its place closes opens from that place.
        let closing = std::mem::take(&mut self.closing);
        let (returning, closing): (Vec<_>, Vec<_>) = closing
            .into_iter()
            .partition(|closing| targets.contains_key(&closing.pane_id));
        self.closing = closing;
        for (pane_id, target) in targets {
            let from = match self.drawn.get(pane_id) {
                Some(drawn) => Some(*drawn),
                None => returning
                    .iter()
                    .find(|closing| closing.pane_id == *pane_id)
                    .map(|closing| between(closing.from, closing.to, closing.ease.at(now)))
                    .or_else(|| self.opening(*target, targets)),
            };
            match from.filter(|from| from != target) {
                Some(from) => {
                    self.moves.insert(*pane_id, Move { from, ease });
                }
                None => {
                    self.moves.remove(pane_id);
                }
            }
        }
        let closing = self
            .drawn
            .iter()
            .filter(|(pane_id, _)| !targets.contains_key(pane_id))
            .filter_map(|(pane_id, from)| {
                let to = self.closed(*from, targets)?;
                Some(Closing {
                    pane_id: *pane_id,
                    from: *from,
                    to,
                    ease,
                })
            })
            .collect::<Vec<_>>();
        self.closing.extend(closing);
    }

    /// Starts `pane_id` alone toward its place in `targets` from where it was last drawn.
    fn follow_raise(
        &mut self,
        pane_id: PaneId,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
        now: Instant,
        motion: spaceterm_ui::ControlMotion,
    ) {
        let from = self.drawn.get(&pane_id).copied();
        match from.zip(targets.get(&pane_id).copied()) {
            Some((from, target)) if from != target => {
                let ease = Ease::new(PRESENCE_DURATION, now, motion);
                self.moves.insert(pane_id, Move { from, ease });
                let area = |bounds: Bounds<Pixels>| {
                    f32::from(bounds.size.width) * f32::from(bounds.size.height)
                };
                let covers = area(target) > area(from);
                // A raise that reverses another fades the Panes beneath from where they are.
                let beneath_from = match self.raised {
                    Some(_) => self.beneath_opacity,
                    None if covers => 1.0,
                    None => 0.0,
                };
                self.raised = Some(Raise {
                    pane_id,
                    covers,
                    beneath_from,
                });
            }
            _ => {
                self.moves.remove(&pane_id);
            }
        }
    }

    /// Where a new Pane laid out at `target` opens from: its far edge from the drawn Pane whose
    /// place it takes part of, which stays in the layout.
    fn opening(
        &self,
        target: Bounds<Pixels>,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
    ) -> Option<Bounds<Pixels>> {
        let center = target.center();
        let neighbor = self
            .drawn
            .iter()
            .find(|(pane_id, drawn)| targets.contains_key(pane_id) && drawn.contains(&center))
            .and_then(|(pane_id, _)| targets.get(pane_id))?;
        far_edge(target, *neighbor)
    }

    /// Where a removed Pane drawn at `from` closes to: its far edge from the drawn Pane that takes
    /// its place.
    fn closed(
        &self,
        from: Bounds<Pixels>,
        targets: &BTreeMap<PaneId, Bounds<Pixels>>,
    ) -> Option<Bounds<Pixels>> {
        let center = from.center();
        let neighbor = targets
            .iter()
            .find(|(pane_id, target)| self.drawn.contains_key(pane_id) && target.contains(&center))
            .and_then(|(pane_id, _)| self.drawn.get(pane_id))?;
        far_edge(from, *neighbor)
    }
}

/// The empty edge of `place` farthest from `neighbor`, beside which it lies.
fn far_edge(place: Bounds<Pixels>, neighbor: Bounds<Pixels>) -> Option<Bounds<Pixels>> {
    let tolerance = px(EDGE_TOLERANCE);
    let vertical_edge = |x| Bounds::new(point(x, place.top()), size(px(0.0), place.size.height));
    let horizontal_edge = |y| Bounds::new(point(place.left(), y), size(place.size.width, px(0.0)));
    if place.left() + tolerance >= neighbor.right() {
        Some(vertical_edge(place.right()))
    } else if place.right() <= neighbor.left() + tolerance {
        Some(vertical_edge(place.left()))
    } else if place.top() + tolerance >= neighbor.bottom() {
        Some(horizontal_edge(place.bottom()))
    } else if place.bottom() <= neighbor.top() + tolerance {
        Some(horizontal_edge(place.top()))
    } else {
        None
    }
}

/// The bounds `progress` of the way from `from` to `to`, edge by edge.
fn between(from: Bounds<Pixels>, to: Bounds<Pixels>, progress: f32) -> Bounds<Pixels> {
    let edge = |from: Pixels, to: Pixels| from + (to - from) * progress;
    Bounds::from_corners(
        point(edge(from.left(), to.left()), edge(from.top(), to.top())),
        point(
            edge(from.right(), to.right()),
            edge(from.bottom(), to.bottom()),
        ),
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use spaceterm_ui::ControlMotion;

    fn rect(left: f32, top: f32, right: f32, bottom: f32) -> Bounds<Pixels> {
        Bounds::from_corners(point(px(left), px(top)), point(px(right), px(bottom)))
    }

    fn layout(panes: &[(u64, Bounds<Pixels>)]) -> BTreeMap<PaneId, Bounds<Pixels>> {
        panes
            .iter()
            .map(|(pane_id, bounds)| (PaneId::new(*pane_id), *bounds))
            .collect()
    }

    fn frame(frames: &PaneFrames, pane_id: u64) -> Bounds<Pixels> {
        frames.frames[&PaneId::new(pane_id)].1
    }

    const HALFWAY: Duration = Duration::from_millis(30);

    #[test]
    fn a_layout_change_nobody_announced_should_draw_every_pane_at_rest() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 100., 50.))]),
            start,
            ControlMotion::Standard,
        );

        let resized = layout(&[(1, rect(0., 0., 200., 50.))]);
        let frames = motion.present_at(&resized, start + HALFWAY, ControlMotion::Standard);

        assert_eq!(
            (frame(&frames, 1), frames.moving),
            (rect(0., 0., 200., 50.), false)
        );
    }

    #[test]
    fn a_split_should_open_the_new_pane_from_its_far_edge_as_its_neighbor_makes_room() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_change();

        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        let opened = motion.present_at(&split, start, ControlMotion::Standard);
        let moving = motion.present_at(&split, start + HALFWAY, ControlMotion::Standard);
        let rested = motion.present_at(&split, start + PRESENCE_DURATION, ControlMotion::Standard);

        assert_eq!(
            (frame(&opened, 1), frame(&opened, 2)),
            (rect(0., 0., 210., 50.), rect(210., 0., 210., 50.)),
            "the first frame must draw the old layout, with the new Pane closed at its far edge"
        );
        let (first, second) = (frame(&moving, 1), frame(&moving, 2));
        assert!(first.right() < px(210.) && first.right() > px(100.));
        assert!(second.left() < px(210.) && second.left() > px(110.));
        assert_eq!(second.right(), px(210.));
        assert_eq!(
            (frame(&rested, 1), frame(&rested, 2), rested.moving),
            (split[&PaneId::new(1)], split[&PaneId::new(2)], false)
        );
    }

    #[test]
    fn a_closed_pane_should_close_toward_its_far_edge_as_its_neighbor_takes_its_place() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&split, start, ControlMotion::Standard);
        motion.expect_change();

        let closed = layout(&[(1, rect(0., 0., 210., 50.))]);
        let first = motion.present_at(&closed, start, ControlMotion::Standard);
        let rested = motion.present_at(&closed, start + PRESENCE_DURATION, ControlMotion::Standard);

        assert_eq!(
            (frame(&first, 1), first.closing.clone()),
            (rect(0., 0., 100., 50.), vec![rect(110., 0., 210., 50.)])
        );
        assert_eq!(
            (frame(&rested, 1), rested.closing.clone(), rested.moving),
            (rect(0., 0., 210., 50.), Vec::new(), false)
        );
    }

    #[test]
    fn a_zoom_should_grow_only_the_zoomed_pane_above_the_others() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        let grid = layout(&[
            (1, rect(0., 0., 100., 50.)),
            (2, rect(110., 0., 210., 50.)),
            (3, rect(0., 60., 100., 110.)),
        ]);
        motion.present_at(&grid, start, ControlMotion::Standard);
        motion.expect_raise(PaneId::new(1));

        let zoomed = layout(&[(1, rect(0., 0., 210., 110.))]);
        let first = motion.present_at(&zoomed, start, ControlMotion::Standard);
        let moving = motion.present_at(&zoomed, start + HALFWAY, ControlMotion::Standard);
        let rested = motion.present_at(&zoomed, start + PRESENCE_DURATION, ControlMotion::Standard);

        assert_eq!(
            (frame(&first, 1), first.closing.clone(), first.raised),
            (
                rect(0., 0., 100., 50.),
                Vec::new(),
                Some((PaneId::new(1), 0.0))
            ),
            "the other Panes must not close; the zoomed Pane covers them"
        );
        let grown = frame(&moving, 1);
        assert!(grown.right() > px(100.) && grown.right() < px(210.));
        assert!(
            moving
                .raised
                .is_some_and(|(_, progress)| progress > 0.0 && progress < 1.0)
        );
        assert_eq!(
            (frame(&rested, 1), rested.raised, rested.moving),
            (zoomed[&PaneId::new(1)], None, false)
        );
    }

    #[test]
    fn a_zoom_that_reverses_an_unzoom_should_fade_the_other_panes_from_where_they_are() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        let zoomed = layout(&[(1, rect(0., 0., 210., 50.))]);
        motion.present_at(&zoomed, start, ControlMotion::Standard);
        motion.expect_raise(PaneId::new(1));
        let grid = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&grid, start, ControlMotion::Standard);
        let unzooming = motion.present_at(
            &grid,
            start + PRESENCE_DURATION / 8,
            ControlMotion::Standard,
        );
        motion.expect_raise(PaneId::new(1));

        let zooming = motion.present_at(
            &zoomed,
            start + PRESENCE_DURATION / 8,
            ControlMotion::Standard,
        );

        assert!(
            unzooming.beneath_opacity > 0.0,
            "{}",
            unzooming.beneath_opacity
        );
        assert_eq!(zooming.beneath_opacity, unzooming.beneath_opacity);
    }

    #[test]
    fn reducing_motion_during_a_change_should_draw_every_pane_at_rest() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_change();
        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&split, start, ControlMotion::Standard);

        let frames = motion.present_at(&split, start + HALFWAY, ControlMotion::Reduced);

        assert_eq!(
            (frame(&frames, 1), frame(&frames, 2), frames.moving),
            (split[&PaneId::new(1)], split[&PaneId::new(2)], false)
        );
    }

    #[test]
    fn a_zoom_should_fade_out_the_other_panes_as_the_zoomed_pane_covers_them() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_raise(PaneId::new(1));

        let zoomed = layout(&[(1, rect(0., 0., 210., 50.))]);
        let opacity = [
            Duration::ZERO,
            PRESENCE_DURATION / 16,
            PRESENCE_DURATION / 2,
        ]
        .map(|at| motion.present_at(&zoomed, start + at, ControlMotion::Standard))
        .map(|frames| frames.beneath_opacity);

        assert_eq!(opacity[0], 1.0, "{opacity:?}");
        assert!(opacity[1] > 0.0 && opacity[1] < 1.0, "{opacity:?}");
        assert_eq!(opacity[2], 0.0, "{opacity:?}");
    }

    #[test]
    fn an_unzoom_should_fade_in_the_other_panes_as_the_zoomed_pane_uncovers_them() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_raise(PaneId::new(1));

        let grid = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        let opacity = [Duration::ZERO, PRESENCE_DURATION / 4, PRESENCE_DURATION]
            .map(|at| motion.present_at(&grid, start + at, ControlMotion::Standard))
            .map(|frames| frames.beneath_opacity);

        assert_eq!(opacity[0], 0.0, "{opacity:?}");
        assert!(opacity[1] > 0.0 && opacity[1] < 1.0, "{opacity:?}");
        assert_eq!(opacity[2], 1.0, "{opacity:?}");
    }

    #[test]
    fn an_unzoom_should_return_the_zoomed_pane_above_the_others_in_their_places() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 110.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_raise(PaneId::new(1));

        let grid = layout(&[
            (1, rect(0., 0., 100., 50.)),
            (2, rect(110., 0., 210., 50.)),
            (3, rect(0., 60., 100., 110.)),
        ]);
        let frames = motion.present_at(&grid, start, ControlMotion::Standard);

        assert_eq!(
            (
                frame(&frames, 1),
                frame(&frames, 2),
                frame(&frames, 3),
                frames.raised.map(|(pane_id, _)| pane_id),
            ),
            (
                rect(0., 0., 210., 110.),
                grid[&PaneId::new(2)],
                grid[&PaneId::new(3)],
                Some(PaneId::new(1)),
            )
        );
    }

    #[test]
    fn a_change_during_motion_should_start_from_where_each_pane_is_drawn() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_change();
        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&split, start, ControlMotion::Standard);
        let drawn = frame(
            &motion.present_at(&split, start + HALFWAY, ControlMotion::Standard),
            1,
        );
        assert!(
            drawn.right() > px(100.) && drawn.right() < px(210.),
            "{drawn:?}"
        );
        motion.expect_change();

        let moved = layout(&[(2, rect(0., 0., 100., 50.)), (1, rect(110., 0., 210., 50.))]);
        let frames = motion.present_at(&moved, start + HALFWAY, ControlMotion::Standard);

        assert_eq!(frame(&frames, 1), drawn);
    }

    #[test]
    fn a_layout_change_nobody_announced_should_stop_the_motion_and_draw_every_pane_at_rest() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Standard,
        );
        motion.expect_change();
        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&split, start, ControlMotion::Standard);

        let resized = layout(&[(1, rect(0., 0., 150., 50.)), (2, rect(160., 0., 310., 50.))]);
        let frames = motion.present_at(&resized, start + HALFWAY, ControlMotion::Standard);

        assert_eq!(
            (frame(&frames, 1), frame(&frames, 2), frames.moving),
            (resized[&PaneId::new(1)], resized[&PaneId::new(2)], false)
        );
    }

    #[test]
    fn a_pane_that_returns_while_its_place_closes_should_open_from_that_place() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        motion.present_at(&split, start, ControlMotion::Standard);
        motion.expect_change();
        let zoomed = layout(&[(1, rect(0., 0., 210., 50.))]);
        motion.present_at(&zoomed, start, ControlMotion::Standard);
        let closing = motion
            .present_at(&zoomed, start + HALFWAY, ControlMotion::Standard)
            .closing;
        motion.expect_change();

        let frames = motion.present_at(&split, start + HALFWAY, ControlMotion::Standard);

        assert_eq!(
            (vec![frame(&frames, 2)], frames.closing),
            (closing, Vec::new()),
            "the returning Pane must replace its closing place"
        );
    }

    #[test]
    fn reduced_motion_should_draw_every_change_at_rest() {
        let start = Instant::now();
        let mut motion = PaneMotion::default();
        motion.present_at(
            &layout(&[(1, rect(0., 0., 210., 50.))]),
            start,
            ControlMotion::Reduced,
        );
        motion.expect_change();

        let split = layout(&[(1, rect(0., 0., 100., 50.)), (2, rect(110., 0., 210., 50.))]);
        let frames = motion.present_at(&split, start, ControlMotion::Reduced);

        assert_eq!(
            (frame(&frames, 1), frame(&frames, 2), frames.moving),
            (split[&PaneId::new(1)], split[&PaneId::new(2)], false)
        );
    }

    #[test]
    fn insets_should_measure_the_drawn_pane_inside_its_laid_out_place() {
        let target = rect(10., 20., 110., 70.);
        let frames = PaneFrames {
            frames: BTreeMap::from([(PaneId::new(1), (target, rect(30., 15., 100., 80.)))]),
            ..PaneFrames::default()
        };

        assert_eq!(
            frames.inset(PaneId::new(1)),
            Edges {
                top: px(-5.),
                right: px(10.),
                bottom: px(-10.),
                left: px(20.),
            }
        );
    }
}
