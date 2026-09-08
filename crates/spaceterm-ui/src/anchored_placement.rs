//! Portable placement policy for transient surfaces attached to a live target.

use gpui::{Bounds, Pixels, Size, point, px, size};

/// Preferred side of an anchored surface relative to its target.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnchoredPlacement {
    /// Place below the target.
    #[default]
    Bottom,
    /// Place above the target.
    Top,
    /// Place to the left of the target.
    Left,
    /// Place to the right of the target.
    Right,
}

/// Cross-axis alignment between an anchored surface and its target.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnchoredAlignment {
    /// Align logical leading edges.
    #[default]
    Start,
    /// Align centers.
    Center,
    /// Align logical trailing edges.
    End,
}

/// Logical horizontal direction used to resolve leading and trailing alignment.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AnchoredTextDirection {
    /// Leading is the left edge.
    #[default]
    LeftToRight,
    /// Leading is the right edge.
    RightToLeft,
}

/// Narrow placement policy shared by anchored transient controls.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchoredPlacementConfig {
    pub(crate) placement: AnchoredPlacement,
    pub(crate) alignment: AnchoredAlignment,
    pub(crate) direction: AnchoredTextDirection,
    pub(crate) offset: Pixels,
    pub(crate) viewport_margin: Pixels,
}

impl AnchoredPlacementConfig {
    /// Creates placement with a four-pixel target offset and twelve-pixel viewport margin.
    pub fn new(placement: AnchoredPlacement, alignment: AnchoredAlignment) -> Self {
        Self {
            placement,
            alignment,
            direction: AnchoredTextDirection::LeftToRight,
            offset: px(4.0),
            viewport_margin: px(12.0),
        }
    }

    /// Sets the logical horizontal direction used by Start and End alignment.
    pub fn direction(mut self, direction: AnchoredTextDirection) -> Self {
        self.direction = direction;
        self
    }

    /// Sets the gap between the target and surface.
    pub fn offset(mut self, offset: Pixels) -> Self {
        self.offset = offset.max(px(0.0));
        self
    }

    /// Sets the minimum distance from the viewport edge.
    pub fn viewport_margin(mut self, margin: Pixels) -> Self {
        self.viewport_margin = margin.max(px(0.0));
        self
    }
}

impl Default for AnchoredPlacementConfig {
    fn default() -> Self {
        Self::new(AnchoredPlacement::default(), AnchoredAlignment::default())
    }
}

pub(crate) fn constrain_anchored_size(
    panel: Size<Pixels>,
    viewport: Size<Pixels>,
    margin: Pixels,
) -> Size<Pixels> {
    let horizontal_margin = margin.min((viewport.width / 2.0).max(px(0.0)));
    let vertical_margin = margin.min((viewport.height / 2.0).max(px(0.0)));
    let available_width = (viewport.width - horizontal_margin * 2.0).max(px(0.0));
    let available_height = (viewport.height - vertical_margin * 2.0).max(px(0.0));
    size(
        panel.width.min(available_width),
        panel.height.min(available_height),
    )
}

pub(crate) fn place_anchored(
    target: Bounds<Pixels>,
    panel: Size<Pixels>,
    viewport: Size<Pixels>,
    config: AnchoredPlacementConfig,
) -> Bounds<Pixels> {
    let margin = config
        .viewport_margin
        .min((viewport.width / 2.0).max(px(0.0)))
        .min((viewport.height / 2.0).max(px(0.0)));
    let max_x = viewport.width - margin;
    let max_y = viewport.height - margin;
    let clamp_x = |x: Pixels| x.max(margin).min((max_x - panel.width).max(margin));
    let clamp_y = |y: Pixels| y.max(margin).min((max_y - panel.height).max(margin));
    let horizontal_alignment = match (config.alignment, config.direction) {
        (AnchoredAlignment::Start, AnchoredTextDirection::LeftToRight)
        | (AnchoredAlignment::End, AnchoredTextDirection::RightToLeft) => target.left(),
        (AnchoredAlignment::End, AnchoredTextDirection::LeftToRight)
        | (AnchoredAlignment::Start, AnchoredTextDirection::RightToLeft) => {
            target.right() - panel.width
        }
        (AnchoredAlignment::Center, _) => target.center().x - panel.width / 2.0,
    };
    let vertical_alignment = match config.alignment {
        AnchoredAlignment::Start => target.top(),
        AnchoredAlignment::Center => target.center().y - panel.height / 2.0,
        AnchoredAlignment::End => target.bottom() - panel.height,
    };
    let (preferred, alternate, vertical) = match config.placement {
        AnchoredPlacement::Bottom => (
            target.bottom() + config.offset,
            target.top() - config.offset - panel.height,
            true,
        ),
        AnchoredPlacement::Top => (
            target.top() - config.offset - panel.height,
            target.bottom() + config.offset,
            true,
        ),
        AnchoredPlacement::Left => (
            target.left() - config.offset - panel.width,
            target.right() + config.offset,
            false,
        ),
        AnchoredPlacement::Right => (
            target.right() + config.offset,
            target.left() - config.offset - panel.width,
            false,
        ),
    };
    if vertical {
        let fits = |y: Pixels| y >= margin && y + panel.height <= max_y;
        let y = if fits(preferred) {
            preferred
        } else if fits(alternate) {
            alternate
        } else {
            clamp_y(preferred)
        };
        Bounds::new(point(clamp_x(horizontal_alignment), y), panel)
    } else {
        let fits = |x: Pixels| x >= margin && x + panel.width <= max_x;
        let x = if fits(preferred) {
            preferred
        } else if fits(alternate) {
            alternate
        } else {
            clamp_x(preferred)
        };
        Bounds::new(point(x, clamp_y(vertical_alignment)), panel)
    }
}

pub(crate) fn place_adjacent(
    parent: Bounds<Pixels>,
    row_top: Pixels,
    panel: Size<Pixels>,
    viewport: Size<Pixels>,
    margin: Pixels,
    gap: Pixels,
) -> Bounds<Pixels> {
    let horizontal_margin = margin.min((viewport.width / 2.0).max(px(0.0)));
    let vertical_margin = margin.min((viewport.height / 2.0).max(px(0.0)));
    let right = parent.right() + gap;
    let left = parent.left() - gap - panel.width;
    let limit_right = viewport.width - horizontal_margin;
    let x = if right + panel.width <= limit_right {
        right
    } else if left >= horizontal_margin {
        left
    } else {
        right
            .max(horizontal_margin)
            .min((limit_right - panel.width).max(horizontal_margin))
    };
    let limit_bottom = viewport.height - vertical_margin;
    let y = row_top
        .max(vertical_margin)
        .min((limit_bottom - panel.height).max(vertical_margin));
    Bounds::new(point(x, y), panel)
}

#[cfg(test)]
mod tests {
    use gpui::{Bounds, point, px, size};

    use super::*;

    #[test]
    fn placement_should_flip_before_clamping() {
        let bounds = place_anchored(
            Bounds::new(point(px(40.0), px(90.0)), size(px(20.0), px(10.0))),
            size(px(60.0), px(40.0)),
            size(px(140.0), px(120.0)),
            AnchoredPlacementConfig::default().viewport_margin(px(8.0)),
        );

        assert_eq!(bounds.top(), px(46.0));
    }

    #[test]
    fn start_alignment_should_follow_logical_direction() {
        let target = Bounds::new(point(px(50.0), px(20.0)), size(px(80.0), px(20.0)));
        let viewport = size(px(240.0), px(180.0));
        let panel = size(px(60.0), px(40.0));

        let ltr = place_anchored(target, panel, viewport, AnchoredPlacementConfig::default());
        let rtl = place_anchored(
            target,
            panel,
            viewport,
            AnchoredPlacementConfig::default().direction(AnchoredTextDirection::RightToLeft),
        );

        assert_eq!((ltr.left(), rtl.left()), (px(50.0), px(70.0)));
    }

    #[test]
    fn placement_should_clamp_both_axes_inside_the_margin() {
        let bounds = place_anchored(
            Bounds::new(point(px(-40.0), px(-20.0)), size(px(10.0), px(10.0))),
            size(px(90.0), px(70.0)),
            size(px(100.0), px(80.0)),
            AnchoredPlacementConfig::default().viewport_margin(px(6.0)),
        );

        assert_eq!(bounds.origin, point(px(6.0), px(6.0)));
    }

    #[test]
    fn placement_should_remain_inside_a_viewport_smaller_than_two_margins() {
        let viewport = size(px(10.0), px(8.0));
        let panel = constrain_anchored_size(size(px(90.0), px(70.0)), viewport, px(12.0));
        let bounds = place_anchored(
            Bounds::new(point(px(0.0), px(0.0)), size(px(1.0), px(1.0))),
            panel,
            viewport,
            AnchoredPlacementConfig::default(),
        );

        assert!(bounds.right() <= viewport.width);
        assert!(bounds.bottom() <= viewport.height);
    }

    #[test]
    fn adjacent_placement_should_remain_inside_a_tiny_viewport() {
        let viewport = size(px(10.0), px(8.0));
        let panel = constrain_anchored_size(size(px(90.0), px(70.0)), viewport, px(12.0));
        let bounds = place_adjacent(
            Bounds::new(point(px(0.0), px(0.0)), size(px(1.0), px(1.0))),
            px(0.0),
            panel,
            viewport,
            px(12.0),
            px(2.0),
        );

        assert!(bounds.right() <= viewport.width);
        assert!(bounds.bottom() <= viewport.height);
    }
}
