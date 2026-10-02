//! Where the Setup Guide docks beside System Settings' window.
//!
//! Every frame here is relative to the top-left corner of one display.

use gpui::{Bounds, Pixels, Point, Size, point, px};

/// The space between System Settings' window and the guide.
const GAP: f32 = 8.0;
/// The widest sidebar System Settings shows. Centering the guide on the content column beside it
/// puts the guide under the privacy list a person drops SpaceTerm onto.
const SIDEBAR_WIDTH: f32 = 215.0;

/// Places the guide outside System Settings' window, preferring the side closest to the privacy
/// list: below, then right, left, and above. When no side fits on the display's visible area, the
/// guide covers the bottom of the window, below the list.
pub(super) fn place_guide(
    settings: Bounds<Pixels>,
    visible: Bounds<Pixels>,
    guide: Size<Pixels>,
) -> Bounds<Pixels> {
    let gap = px(GAP);
    let content_left = settings.left() + px(SIDEBAR_WIDTH.min(settings.size.width.as_f32() / 3.0));
    let column_x = content_left + (settings.right() - content_left - guide.width) / 2.0;
    let middle_y = settings.top() + (settings.size.height - guide.height) / 2.0;
    let below = point(column_x, settings.bottom() + gap);
    let right = point(settings.right() + gap, middle_y);
    let left = point(settings.left() - gap - guide.width, middle_y);
    let above = point(column_x, settings.top() - gap - guide.height);
    [
        (below, Axis::Horizontal),
        (right, Axis::Vertical),
        (left, Axis::Vertical),
        (above, Axis::Horizontal),
    ]
    .into_iter()
    .map(|(origin, slide)| Bounds::new(slide.clamp(origin, guide, visible), guide))
    .find(|bounds| fits(*bounds, visible))
    .unwrap_or_else(|| {
        let covering = point(column_x, settings.bottom() - gap - guide.height);
        let covering = Axis::Vertical.clamp(Axis::Horizontal.clamp(covering, guide, visible), guide, visible);
        Bounds::new(covering, guide)
    })
}

/// Whether `bounds` lies on `visible`, edges included.
fn fits(bounds: Bounds<Pixels>, visible: Bounds<Pixels>) -> bool {
    bounds.left() >= visible.left()
        && bounds.top() >= visible.top()
        && bounds.right() <= visible.right()
        && bounds.bottom() <= visible.bottom()
}

/// The axis along which a candidate may slide to stay on the display.
#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

impl Axis {
    fn clamp(self, origin: Point<Pixels>, guide: Size<Pixels>, visible: Bounds<Pixels>) -> Point<Pixels> {
        let slide = |start: Pixels, length: Pixels, low: Pixels, high: Pixels| {
            // A guide longer than the visible area keeps its leading edge visible.
            px(start.as_f32().min((high - length).as_f32()).max(low.as_f32()))
        };
        match self {
            Self::Horizontal => point(
                slide(origin.x, guide.width, visible.left(), visible.right()),
                origin.y,
            ),
            Self::Vertical => point(
                origin.x,
                slide(origin.y, guide.height, visible.top(), visible.bottom()),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{bounds, size};

    use super::*;

    const GUIDE: Size<Pixels> = Size {
        width: px(360.0),
        height: px(180.0),
    };

    fn visible() -> Bounds<Pixels> {
        bounds(point(px(0.0), px(25.0)), size(px(1512.0), px(920.0)))
    }

    #[test]
    fn the_guide_docks_below_the_content_column() {
        let settings = bounds(point(px(300.0), px(100.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(settings, visible(), GUIDE);

        assert_eq!(placed.top(), px(608.0));
        // The column starts after the sidebar, so the guide centers under the list.
        assert_eq!(placed.left(), px(300.0 + 215.0 + (500.0 - 360.0) / 2.0));
    }

    #[test]
    fn the_guide_slides_along_the_bottom_edge_to_stay_on_the_display() {
        let settings = bounds(point(px(1100.0), px(100.0)), size(px(715.0), px(500.0)));

        let placed = place_guide(settings, visible(), GUIDE);

        assert_eq!(placed.top(), px(608.0));
        assert_eq!(placed.right(), px(1512.0));
    }

    #[test]
    fn a_window_reaching_the_bottom_puts_the_guide_beside_it() {
        let settings = bounds(point(px(200.0), px(300.0)), size(px(715.0), px(640.0)));

        let placed = place_guide(settings, visible(), GUIDE);

        assert_eq!(placed.left(), px(200.0 + 715.0 + 8.0));
        assert_eq!(placed.top(), px(300.0 + (640.0 - 180.0) / 2.0));
    }

    #[test]
    fn a_window_reaching_the_bottom_and_right_puts_the_guide_on_its_left() {
        let settings = bounds(point(px(700.0), px(300.0)), size(px(812.0), px(640.0)));

        let placed = place_guide(settings, visible(), GUIDE);

        assert_eq!(placed.right(), px(700.0 - 8.0));
    }

    #[test]
    fn a_window_filling_the_display_is_covered_at_its_bottom() {
        let settings = visible();

        let placed = place_guide(settings, visible(), GUIDE);

        assert!(fits(placed, settings));
        assert_eq!(placed.bottom(), settings.bottom() - px(8.0));
    }
}
