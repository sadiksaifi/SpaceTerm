//! Portable placement policy for transient surfaces attached to a live target.

use gpui::{Bounds, Pixels, Size, point, px, size};
use std::ops::Range;

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

/// Placement preferences shared by anchored transient controls.
///
/// Surfaces keep the preferred side when they fit, then try the opposite side.
/// If neither side fits, they shrink to the side with more room. Alignment can
/// flip or shift along the other axis to keep the surface inside the viewport.
/// When the target leaves no space on either preferred-axis side, placement
/// falls back to the perpendicular axis if it has any room.
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
    let available = inset_viewport(viewport, margin);
    size(
        panel.width.max(px(0.0)).min(available.size.width),
        panel.height.max(px(0.0)).min(available.size.height),
    )
}

fn inset_viewport(viewport: Size<Pixels>, margin: Pixels) -> Bounds<Pixels> {
    let width = viewport.width.max(px(0.0));
    let height = viewport.height.max(px(0.0));
    let horizontal_margin = margin.max(px(0.0)).min(width / 2.0);
    let vertical_margin = margin.max(px(0.0)).min(height / 2.0);
    Bounds::new(
        point(horizontal_margin, vertical_margin),
        size(
            width - horizontal_margin * 2.0,
            height - vertical_margin * 2.0,
        ),
    )
}

/// Resolves placement and the actual surface size together. Callers must lay out
/// scrollable content using the returned size rather than the requested size.
pub(crate) fn place_anchored(
    target: Bounds<Pixels>,
    panel: Size<Pixels>,
    viewport: Size<Pixels>,
    config: AnchoredPlacementConfig,
) -> Bounds<Pixels> {
    let available = inset_viewport(viewport, config.viewport_margin);
    let mut panel = constrain_anchored_size(panel, viewport, config.viewport_margin);
    let vertical = matches!(
        config.placement,
        AnchoredPlacement::Top | AnchoredPlacement::Bottom
    );
    let (target_axis, viewport_axis, requested_length) = if vertical {
        (
            target.top()..target.bottom(),
            available.top()..available.bottom(),
            panel.height,
        )
    } else {
        (
            target.left()..target.right(),
            available.left()..available.right(),
            panel.width,
        )
    };
    let before_end =
        (target_axis.start - config.offset).clamp(viewport_axis.start, viewport_axis.end);
    let after_start =
        (target_axis.end + config.offset).clamp(viewport_axis.start, viewport_axis.end);
    let before_space = before_end - viewport_axis.start;
    let after_space = viewport_axis.end - after_start;
    if before_space == px(0.0) && after_space == px(0.0) {
        let (other_target, other_viewport, fallback) = if vertical {
            (
                target.left()..target.right(),
                available.left()..available.right(),
                match config.direction {
                    AnchoredTextDirection::LeftToRight => AnchoredPlacement::Right,
                    AnchoredTextDirection::RightToLeft => AnchoredPlacement::Left,
                },
            )
        } else {
            (
                target.top()..target.bottom(),
                available.top()..available.bottom(),
                AnchoredPlacement::Bottom,
            )
        };
        if (other_target.start - config.offset).clamp(other_viewport.start, other_viewport.end)
            > other_viewport.start
            || (other_target.end + config.offset).clamp(other_viewport.start, other_viewport.end)
                < other_viewport.end
        {
            // Positive space on the other axis makes this fallback a single step.
            return place_anchored(
                target,
                panel,
                viewport,
                AnchoredPlacementConfig {
                    placement: fallback,
                    ..config
                },
            );
        }
    }
    let prefer_before = matches!(
        config.placement,
        AnchoredPlacement::Top | AnchoredPlacement::Left
    );
    let (preferred_space, alternate_space) = if prefer_before {
        (before_space, after_space)
    } else {
        (after_space, before_space)
    };
    let use_preferred = preferred_space >= requested_length || preferred_space >= alternate_space;
    let place_before = prefer_before == use_preferred;
    let length = requested_length.min(if place_before {
        before_space
    } else {
        after_space
    });
    let origin = if place_before {
        before_end - length
    } else {
        after_start
    };
    if vertical {
        panel.height = length;
        let alignment = match (config.alignment, config.direction) {
            (AnchoredAlignment::Start, AnchoredTextDirection::RightToLeft) => {
                AnchoredAlignment::End
            }
            (AnchoredAlignment::End, AnchoredTextDirection::RightToLeft) => {
                AnchoredAlignment::Start
            }
            (alignment, _) => alignment,
        };
        let x = align_on_axis(
            target.left()..target.right(),
            panel.width,
            available.left()..available.right(),
            alignment,
        );
        Bounds::new(point(x, origin), panel)
    } else {
        panel.width = length;
        let y = align_on_axis(
            target.top()..target.bottom(),
            panel.height,
            available.top()..available.bottom(),
            config.alignment,
        );
        Bounds::new(point(origin, y), panel)
    }
}

fn align_on_axis(
    target: Range<Pixels>,
    length: Pixels,
    viewport: Range<Pixels>,
    alignment: AnchoredAlignment,
) -> Pixels {
    let (preferred, alternate) = match alignment {
        AnchoredAlignment::Start => (target.start, target.end - length),
        AnchoredAlignment::End => (target.end - length, target.start),
        AnchoredAlignment::Center => {
            let center = (target.start + target.end - length) / 2.0;
            (center, center)
        }
    };
    let fits = |origin: Pixels| origin >= viewport.start && origin + length <= viewport.end;
    if fits(preferred) {
        preferred
    } else if fits(alternate) {
        alternate
    } else {
        preferred.clamp(viewport.start, (viewport.end - length).max(viewport.start))
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
    fn every_side_should_flip_when_only_the_opposite_side_fits() {
        for (placement, target_origin, expected_origin) in [
            (AnchoredPlacement::Left, (8.0, 50.0), (32.0, 50.0)),
            (AnchoredPlacement::Right, (172.0, 50.0), (108.0, 50.0)),
            (AnchoredPlacement::Top, (50.0, 8.0), (50.0, 32.0)),
            (AnchoredPlacement::Bottom, (50.0, 172.0), (50.0, 128.0)),
        ] {
            let bounds = place_anchored(
                Bounds::new(
                    point(px(target_origin.0), px(target_origin.1)),
                    size(px(20.0), px(20.0)),
                ),
                size(px(60.0), px(40.0)),
                size(px(200.0), px(200.0)),
                AnchoredPlacementConfig::new(placement, AnchoredAlignment::Start)
                    .viewport_margin(px(8.0)),
            );

            assert_eq!(
                bounds,
                Bounds::new(
                    point(px(expected_origin.0), px(expected_origin.1)),
                    size(px(60.0), px(40.0)),
                ),
                "{placement:?}",
            );
        }
    }

    #[test]
    fn preferred_side_should_remain_stable_when_both_sides_fit() {
        for (placement, expected_origin) in [
            (AnchoredPlacement::Left, (36.0, 80.0)),
            (AnchoredPlacement::Right, (104.0, 80.0)),
            (AnchoredPlacement::Top, (80.0, 36.0)),
            (AnchoredPlacement::Bottom, (80.0, 104.0)),
        ] {
            let bounds = place_anchored(
                Bounds::new(point(px(80.0), px(80.0)), size(px(20.0), px(20.0))),
                size(px(40.0), px(40.0)),
                size(px(200.0), px(200.0)),
                AnchoredPlacementConfig::new(placement, AnchoredAlignment::Start),
            );

            assert_eq!(
                bounds.origin,
                point(px(expected_origin.0), px(expected_origin.1)),
                "{placement:?}",
            );
        }
    }

    #[test]
    fn neither_side_fitting_should_shrink_into_the_larger_space_without_covering_the_target() {
        for (placement, target_origin, expected_origin, expected_size) in [
            (
                AnchoredPlacement::Top,
                (40.0, 80.0),
                (40.0, 104.0),
                (120.0, 88.0),
            ),
            (
                AnchoredPlacement::Bottom,
                (40.0, 100.0),
                (40.0, 8.0),
                (120.0, 88.0),
            ),
            (
                AnchoredPlacement::Left,
                (80.0, 40.0),
                (104.0, 40.0),
                (88.0, 120.0),
            ),
            (
                AnchoredPlacement::Right,
                (100.0, 40.0),
                (8.0, 40.0),
                (88.0, 120.0),
            ),
        ] {
            let bounds = place_anchored(
                Bounds::new(
                    point(px(target_origin.0), px(target_origin.1)),
                    size(px(20.0), px(20.0)),
                ),
                size(px(120.0), px(120.0)),
                size(px(200.0), px(200.0)),
                AnchoredPlacementConfig::new(placement, AnchoredAlignment::Start)
                    .viewport_margin(px(8.0)),
            );

            assert_eq!(
                bounds,
                Bounds::new(
                    point(px(expected_origin.0), px(expected_origin.1)),
                    size(px(expected_size.0), px(expected_size.1)),
                ),
                "{placement:?}",
            );
        }
    }

    #[test]
    fn equal_insufficient_space_should_keep_the_preferred_side() {
        let target = Bounds::new(point(px(90.0), px(90.0)), size(px(20.0), px(20.0)));
        let panel = size(px(120.0), px(120.0));
        let viewport = size(px(200.0), px(200.0));
        let top = place_anchored(
            target,
            panel,
            viewport,
            AnchoredPlacementConfig::new(AnchoredPlacement::Top, AnchoredAlignment::Center),
        );
        let bottom = place_anchored(target, panel, viewport, AnchoredPlacementConfig::default());

        assert_eq!((top.bottom(), bottom.top()), (px(86.0), px(114.0)));
    }

    #[test]
    fn a_target_spanning_the_preferred_axis_should_use_perpendicular_space() {
        for (placement, target, expected) in [
            (
                AnchoredPlacement::Top,
                Bounds::new(point(px(80.0), px(0.0)), size(px(20.0), px(200.0))),
                Bounds::new(point(px(104.0), px(8.0)), size(px(60.0), px(40.0))),
            ),
            (
                AnchoredPlacement::Bottom,
                Bounds::new(point(px(160.0), px(0.0)), size(px(20.0), px(200.0))),
                Bounds::new(point(px(96.0), px(8.0)), size(px(60.0), px(40.0))),
            ),
            (
                AnchoredPlacement::Left,
                Bounds::new(point(px(0.0), px(80.0)), size(px(200.0), px(20.0))),
                Bounds::new(point(px(8.0), px(104.0)), size(px(60.0), px(40.0))),
            ),
        ] {
            let bounds = place_anchored(
                target,
                size(px(60.0), px(40.0)),
                size(px(200.0), px(200.0)),
                AnchoredPlacementConfig::new(placement, AnchoredAlignment::Start)
                    .viewport_margin(px(8.0)),
            );

            assert_eq!(bounds, expected, "{placement:?}");
        }
    }

    #[test]
    fn corners_should_flip_the_side_and_alignment_before_shifting() {
        for (target_origin, alignment, expected_origin) in [
            ((8.0, 8.0), AnchoredAlignment::End, (8.0, 32.0)),
            ((170.0, 8.0), AnchoredAlignment::Start, (130.0, 32.0)),
            ((8.0, 172.0), AnchoredAlignment::End, (8.0, 128.0)),
            ((170.0, 172.0), AnchoredAlignment::Start, (130.0, 128.0)),
        ] {
            let bounds = place_anchored(
                Bounds::new(
                    point(px(target_origin.0), px(target_origin.1)),
                    size(px(20.0), px(20.0)),
                ),
                size(px(60.0), px(40.0)),
                size(px(200.0), px(200.0)),
                AnchoredPlacementConfig::new(AnchoredPlacement::Bottom, alignment)
                    .viewport_margin(px(8.0)),
            );

            assert_eq!(
                bounds.origin,
                point(px(expected_origin.0), px(expected_origin.1)),
                "target {target_origin:?}",
            );
        }
    }

    #[test]
    fn alignment_should_shift_when_neither_edge_alignment_fits() {
        let bounds = place_anchored(
            Bounds::new(point(px(70.0), px(40.0)), size(px(60.0), px(20.0))),
            size(px(160.0), px(40.0)),
            size(px(200.0), px(200.0)),
            AnchoredPlacementConfig::default().viewport_margin(px(8.0)),
        );

        assert_eq!(bounds.origin, point(px(32.0), px(64.0)));
    }

    #[test]
    fn resizing_should_flip_shrink_and_restore_the_requested_size() {
        let target = Bounds::new(point(px(80.0), px(80.0)), size(px(20.0), px(20.0)));
        let requested = size(px(60.0), px(80.0));
        for (height, expected_y, expected_height) in [
            (220.0, 104.0, 80.0),
            (140.0, 12.0, 64.0),
            (220.0, 104.0, 80.0),
        ] {
            let bounds = place_anchored(
                target,
                requested,
                size(px(220.0), px(height)),
                AnchoredPlacementConfig::default(),
            );

            assert_eq!(
                (bounds.top(), bounds.size.height),
                (px(expected_y), px(expected_height)),
                "viewport height {height}",
            );
        }
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
    fn rtl_should_reverse_horizontal_alignment_without_reversing_physical_sides() {
        for (placement, alignment, expected_origin) in [
            (
                AnchoredPlacement::Bottom,
                AnchoredAlignment::Start,
                (40.0, 124.0),
            ),
            (
                AnchoredPlacement::Bottom,
                AnchoredAlignment::End,
                (80.0, 124.0),
            ),
            (
                AnchoredPlacement::Right,
                AnchoredAlignment::Start,
                (124.0, 80.0),
            ),
            (
                AnchoredPlacement::Right,
                AnchoredAlignment::End,
                (124.0, 100.0),
            ),
        ] {
            let bounds = place_anchored(
                Bounds::new(point(px(80.0), px(80.0)), size(px(40.0), px(40.0))),
                size(px(80.0), px(20.0)),
                size(px(240.0), px(240.0)),
                AnchoredPlacementConfig::new(placement, alignment)
                    .direction(AnchoredTextDirection::RightToLeft),
            );

            assert_eq!(
                bounds.origin,
                point(px(expected_origin.0), px(expected_origin.1)),
                "{placement:?} {alignment:?}",
            );
        }
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
    fn unmeasured_oversized_panels_should_stay_contained_at_every_edge_and_viewport_size() {
        for width in [0.0, 1.0, 8.0, 20.0, 100.0, 240.0] {
            for height in [0.0, 1.0, 8.0, 20.0, 100.0, 240.0] {
                for (x, y) in [
                    (-30.0, -30.0),
                    (0.0, 0.0),
                    (width / 2.0, height / 2.0),
                    (width, height),
                ] {
                    for placement in [
                        AnchoredPlacement::Top,
                        AnchoredPlacement::Bottom,
                        AnchoredPlacement::Left,
                        AnchoredPlacement::Right,
                    ] {
                        let bounds = place_anchored(
                            Bounds::new(point(px(x), px(y)), size(px(20.0), px(20.0))),
                            size(px(300.0), px(300.0)),
                            size(px(width), px(height)),
                            AnchoredPlacementConfig::new(placement, AnchoredAlignment::Start),
                        );

                        assert!(
                            bounds.left() >= px(0.0)
                                && bounds.top() >= px(0.0)
                                && bounds.right() <= px(width)
                                && bounds.bottom() <= px(height)
                                && bounds.size.width >= px(0.0)
                                && bounds.size.height >= px(0.0),
                            "{bounds:?} overflows {width}x{height}, target {x},{y}, {placement:?}",
                        );
                    }
                }
            }
        }
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
