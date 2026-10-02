//! Where the Setup Guide docks on System Settings' window.
//!
//! Every frame here is relative to the top-left corner of one display.

use gpui::{Bounds, Pixels, point, px, size};

/// The space between the guide's sides and the content column's edges, which lines the guide up
/// with the privacy list's rows.
const SIDE_INSET: f32 = 20.0;
/// The space between the guide and the bottom of System Settings' window.
const BOTTOM_INSET: f32 = 12.0;
/// The width of System Settings' sidebar. The content column beside it holds the privacy list a
/// person drops SpaceTerm onto.
const SIDEBAR_WIDTH: f32 = 232.0;
/// The guide's width limits. Past the widest, the instruction gains nothing; below the narrowest,
/// it truncates.
const MIN_WIDTH: f32 = 300.0;
const MAX_WIDTH: f32 = 560.0;

/// Places the guide inside the bottom of System Settings' content column, directly below the
/// privacy list, so "the list above" is where the guide points. The guide stays on the display's
/// visible area when the window extends past it.
pub(super) fn place_guide(
    settings: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    height: Pixels,
) -> Bounds<Pixels> {
    let content_left = settings.left() + px(SIDEBAR_WIDTH.min(settings.size.width.as_f32() / 3.0));
    let column = settings.right() - content_left;
    let width = (column - px(SIDE_INSET) * 2.0).clamp(px(MIN_WIDTH), px(MAX_WIDTH));
    let left = content_left + (column - width) / 2.0;
    let top = settings.bottom() - px(BOTTOM_INSET) - height;
    let clamp = |start: Pixels, length: Pixels, low: Pixels, high: Pixels| {
        // A guide longer than the visible area keeps its leading edge visible.
        start.min(high - length).max(low)
    };
    Bounds::new(
        point(
            clamp(left, width, visible.left(), visible.right()),
            clamp(top, height, visible.top(), visible.bottom()),
        ),
        size(width, height),
    )
}

#[cfg(test)]
mod tests {
    use gpui::bounds;

    use super::*;

    const HEIGHT: Pixels = px(92.0);

    fn visible() -> Bounds<Pixels> {
        bounds(point(px(0.0), px(25.0)), size(px(1512.0), px(920.0)))
    }

    #[test]
    fn the_guide_docks_inside_the_bottom_of_the_content_column() {
        let settings = bounds(point(px(300.0), px(100.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(settings, visible(), HEIGHT);

        assert_eq!(placed.bottom(), px(600.0 - 12.0));
        assert_eq!(placed.left(), px(300.0 + 232.0 + 20.0));
        assert_eq!(placed.right(), px(1015.0 - 20.0));
    }

    #[test]
    fn a_wide_window_centers_the_guide_on_the_content_column() {
        let settings = bounds(point(px(100.0), px(100.0)), size(px(1232.0), px(500.0)));

        let placed = place_guide(settings, visible(), HEIGHT);

        assert_eq!(placed.size.width, px(MAX_WIDTH));
        assert_eq!(placed.left(), px(100.0 + 232.0 + (1000.0 - MAX_WIDTH) / 2.0));
    }

    #[test]
    fn a_window_past_the_display_keeps_the_guide_on_the_display() {
        let settings = bounds(point(px(1100.0), px(600.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(settings, visible(), HEIGHT);

        assert_eq!(placed.right(), px(1512.0));
        assert_eq!(placed.bottom(), px(945.0));
    }
}
