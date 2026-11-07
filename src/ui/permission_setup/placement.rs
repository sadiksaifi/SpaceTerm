//! Where the Setup Guide docks on System Settings' window.
//! Every frame here is relative to the top-left corner of one display.

use gpui::{Bounds, Pixels, point, px, size};

/// The space between the guide's sides and the window's edges when the guide matches its width.
const SIDE_INSET: f32 = 20.0;
/// The space between the guide and the window it docks against.
const GAP: f32 = 8.0;
/// How far the guide's bottom edge sits above the window's when the guide is level with it.
const BOTTOM_INSET: f32 = 12.0;
/// The guide's width limits. Past the widest, the instruction gains nothing; below the narrowest,
/// it truncates.
const MIN_WIDTH: f32 = 300.0;
const MAX_WIDTH: f32 = 560.0;

/// Which way the guide's arrow points to reach System Settings' window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Pointing {
    Up,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct GuidePlacement {
    pub(super) bounds: Bounds<Pixels>,
    pub(super) pointing: Pointing,
}

/// Places the guide outside System Settings' window, so it never covers the privacy list whose
/// entry it names. System Settings sorts that list by name, so the entry can be any row. The guide
/// docks below the window, or beside it when the display has no room below, and stays on the
/// display's visible area. Only a window that leaves no room on any of those sides gets the guide
/// inside its bottom edge.
pub(super) fn place_guide(
    window: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    height: Pixels,
) -> GuidePlacement {
    let gap = px(GAP);
    let fitted = |available: Pixels| available.min(px(MAX_WIDTH));
    // A guide longer than the visible area keeps its leading edge visible.
    let clamp = |start: Pixels, length: Pixels, low: Pixels, high: Pixels| {
        start.min(high - length).max(low)
    };
    // Level with the bottom of the window, where the list ends.
    let level_top = clamp(
        window.bottom() - px(BOTTOM_INSET) - height,
        height,
        visible.top(),
        visible.bottom(),
    );
    let centered = |top: Pixels, pointing: Pointing| {
        let width = (window.size.width - px(SIDE_INSET) * 2.0).clamp(px(MIN_WIDTH), px(MAX_WIDTH));
        let left = window.left() + (window.size.width - width) / 2.0;
        GuidePlacement {
            bounds: Bounds::new(
                point(clamp(left, width, visible.left(), visible.right()), top),
                size(width, height),
            ),
            pointing,
        }
    };

    let below = window.bottom() + gap;
    if below + height <= visible.bottom() {
        return centered(below.max(visible.top()), Pointing::Up);
    }
    let right = window.right() + gap;
    let right_room = visible.right() - right;
    if right_room >= px(MIN_WIDTH) {
        return GuidePlacement {
            bounds: Bounds::new(point(right, level_top), size(fitted(right_room), height)),
            pointing: Pointing::Left,
        };
    }
    let left_room = window.left() - gap - visible.left();
    if left_room >= px(MIN_WIDTH) {
        let width = fitted(left_room);
        return GuidePlacement {
            bounds: Bounds::new(
                point(window.left() - gap - width, level_top),
                size(width, height),
            ),
            pointing: Pointing::Right,
        };
    }
    centered(level_top, Pointing::Up)
}

#[cfg(test)]
mod tests {
    use gpui::bounds;

    use super::*;

    const HEIGHT: Pixels = px(92.0);

    fn visible() -> Bounds<Pixels> {
        bounds(point(px(0.0), px(25.0)), size(px(1512.0), px(920.0)))
    }

    fn overlaps(first: Bounds<Pixels>, second: Bounds<Pixels>) -> bool {
        first.left() < second.right()
            && second.left() < first.right()
            && first.top() < second.bottom()
            && second.top() < first.bottom()
    }

    fn contains(outer: Bounds<Pixels>, inner: Bounds<Pixels>) -> bool {
        inner.left() >= outer.left()
            && inner.right() <= outer.right()
            && inner.top() >= outer.top()
            && inner.bottom() <= outer.bottom()
    }

    #[test]
    fn the_guide_docks_below_the_window_when_the_display_has_room() {
        let window = bounds(point(px(300.0), px(100.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.pointing, Pointing::Up);
        assert_eq!(placed.bounds.top(), px(600.0 + 8.0));
        assert_eq!(placed.bounds.size.width, px(560.0));
        assert_eq!(
            placed.bounds.left(),
            px(300.0 + (715.0 - 560.0) / 2.0),
            "centered under the window"
        );
        assert!(!overlaps(placed.bounds, window));
    }

    #[test]
    fn a_narrow_window_gets_a_guide_as_wide_as_its_list() {
        let window = bounds(point(px(300.0), px(100.0)), size(px(450.0), px(500.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.bounds.size.width, px(450.0 - 40.0));
        assert_eq!(placed.bounds.left(), px(320.0));
    }

    #[test]
    fn a_window_reaching_the_bottom_of_the_display_gets_the_guide_beside_it() {
        let window = bounds(point(px(200.0), px(300.0)), size(px(715.0), px(640.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.pointing, Pointing::Left);
        assert_eq!(placed.bounds.left(), px(915.0 + 8.0));
        assert_eq!(
            placed.bounds.bottom(),
            px(940.0 - 12.0),
            "level with the end of the list"
        );
        assert!(!overlaps(placed.bounds, window));
        assert!(contains(visible(), placed.bounds));
    }

    #[test]
    fn a_window_against_the_right_edge_gets_the_guide_on_its_left() {
        let window = bounds(point(px(780.0), px(300.0)), size(px(715.0), px(640.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.pointing, Pointing::Right);
        assert_eq!(placed.bounds.right(), px(780.0 - 8.0));
        assert_eq!(placed.bounds.size.width, px(560.0));
        assert!(!overlaps(placed.bounds, window));
        assert!(contains(visible(), placed.bounds));
    }

    #[test]
    fn the_side_guide_narrows_to_the_room_beside_the_window() {
        let window = bounds(point(px(400.0), px(300.0)), size(px(715.0), px(640.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.pointing, Pointing::Left);
        assert_eq!(placed.bounds.right(), px(1512.0));
        assert_eq!(placed.bounds.size.width, px(1512.0 - 1115.0 - 8.0));
    }

    #[test]
    fn a_window_filling_the_display_keeps_the_guide_inside_its_bottom_edge() {
        let window = bounds(point(px(0.0), px(25.0)), size(px(1512.0), px(920.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert_eq!(placed.pointing, Pointing::Up);
        assert_eq!(placed.bounds.bottom(), px(945.0 - 12.0));
        assert!(contains(visible(), placed.bounds));
    }

    #[test]
    fn a_window_past_the_display_keeps_the_guide_on_the_display() {
        let window = bounds(point(px(1300.0), px(700.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(window, visible(), HEIGHT);

        assert!(contains(visible(), placed.bounds));
    }
}
