//! Shared Workspace gaps, optical Split trim, native edge insets, and derived Pane radii.
//! Split gaps are two points narrower because the lighter band and adjacent rounded corners make
//! them appear wider.

use gpui::{App, Bounds, Pixels, point, px};

use crate::appearance::{ChromeColors, Color};
use crate::platform::window_frame::WindowFrameGeometry;
use crate::ui::chrome_geometry::{RadiusRole, pane_radius};

/// The one continuous Chrome surface beneath the Workspace: the sidebar region, the content stage
/// insets, and the gaps between Panes.
pub(crate) fn base_surface(colors: &ChromeColors) -> Color {
    colors.panel_background
}

/// The Compact-density measurement of every visible gap in the Workspace.
const SPACE: f32 = 5.0;
/// How much shorter a top chip is than the base top-chrome height, at Compact density.
const TOP_CHIP_TRIM: f32 = 6.0;
/// How much narrower the gap between Split Panes is than every other gap, at every density.
const SPLIT_GAP_OPTICAL_TRIM: f32 = 2.0;
/// Used when the hosting platform cannot supply an outer window radius.
const FALLBACK_WINDOW_RADIUS: f32 = 12.0;
/// Width of the hairline rim each Pane paints over its own edge.
const PANE_RIM_WIDTH: f32 = 1.0;

/// Resolved whole-point geometry of the floating Workspace frame for one density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkspaceFrame {
    space: Pixels,
    window_edge: Pixels,
    strip_chip_leading_inset: Pixels,
    top_chip_trim: Pixels,
    pane_radius: Pixels,
    chip_radius: Pixels,
}

/// Paint geometry inside a Pane's rim, with the terminal's top edge below the Pane Caption.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PanePaintInterior {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) corner_radius: Pixels,
}

impl WorkspaceFrame {
    /// Resolves the frame for a Chrome density scale and the hosting window's geometry. Lengths
    /// round to whole points so Comfortable density never leaves a blurry edge.
    pub(crate) fn resolve(spacing_scale: f32, window: WindowFrameGeometry) -> Self {
        let scale = if spacing_scale.is_finite() && spacing_scale > 0.0 {
            spacing_scale
        } else {
            1.0
        };
        let space = (SPACE * scale).round();
        let window_edge = match window.outer_edge_width() {
            width if width.is_finite() && width >= 0.0 => width.round(),
            _ => 0.0,
        };
        // A Pane radius follows the native window corner carried inward by its inset.
        let chip_radius = RadiusRole::Control.points();
        let window_radius = match window.outer_corner_radius() {
            Some(radius) if radius.is_finite() && radius >= 0.0 => radius,
            Some(_) | None => FALLBACK_WINDOW_RADIUS,
        };
        let pane_radius = pane_radius(window_radius, space + window_edge);
        Self {
            space: px(space),
            window_edge: px(window_edge),
            strip_chip_leading_inset: px((space / 2.0).floor()),
            top_chip_trim: px((TOP_CHIP_TRIM * scale).round()),
            pane_radius: px(pane_radius),
            chip_radius: px(chip_radius),
        }
    }

    /// The frame for the installed Chrome appearance and host-supplied window geometry.
    pub(crate) fn for_appearance(
        appearance: &super::appearance::ChromeAppearance,
        cx: &App,
    ) -> Self {
        let window = cx
            .try_global::<WindowFrameGeometry>()
            .copied()
            .unwrap_or_default();
        Self::resolve(appearance.spacing_scale, window)
    }

    /// The one visible gap in the Workspace: stage perimeter, Split gap, and sidebar chip margin.
    /// Two insets of this size may never meet, or they read as one gap of twice the size.
    pub(crate) const fn space(self) -> Pixels {
        self.space
    }

    /// The width of the edge the window paints over the outermost point of its content.
    pub(crate) const fn window_edge(self) -> Pixels {
        self.window_edge
    }

    pub(crate) fn window_edge_inset(self) -> Pixels {
        self.window_edge + self.space
    }

    /// What a chip in a strip keeps on its leading side of the gap to its neighbour. Two shares
    /// split one whole-point gap, so the visible gap stays [`WorkspaceFrame::space`] even when it
    /// is odd.
    pub(crate) const fn strip_chip_leading_inset(self) -> Pixels {
        self.strip_chip_leading_inset
    }

    pub(crate) fn strip_chip_trailing_inset(self) -> Pixels {
        self.space - self.strip_chip_leading_inset
    }

    /// Base surface between two Split Panes; it also owns the Split's resize hit target.
    pub(crate) fn pane_gap(self) -> Pixels {
        self.space - px(SPLIT_GAP_OPTICAL_TRIM)
    }

    pub(crate) fn sidebar_chip_leading_inset(self) -> Pixels {
        self.window_edge_inset()
    }

    /// The margin a sidebar item's chip keeps on its trailing side, facing the Pane.
    pub(crate) const fn sidebar_chip_trailing_inset(self) -> Pixels {
        self.space
    }

    /// The Pane stage's leading perimeter. With a sidebar present the sidebar chip's trailing
    /// margin already paints that gap, so the stage adds nothing.
    pub(crate) fn stage_leading_inset(self, sidebar_visible: bool) -> Pixels {
        if sidebar_visible {
            px(0.0)
        } else {
            self.window_edge_inset()
        }
    }

    /// The height of the top chrome, which absorbs the frame's top space.
    pub(crate) fn top_chrome_height(self, base_height: Pixels) -> Pixels {
        self.window_edge + self.top_band_height(base_height)
    }

    /// The part of the top chrome beneath the window's own edge, where every top control centres.
    pub(crate) fn top_band_height(self, base_height: Pixels) -> Pixels {
        self.space + self.top_chip_height(base_height) + self.space
    }

    /// The height of a chip riding the top chrome's band: the Active Tab and the collapsed switcher.
    pub(crate) fn top_chip_height(self, base_height: Pixels) -> Pixels {
        base_height - self.top_chip_trim
    }

    pub(crate) const fn pane_radius(self) -> Pixels {
        self.pane_radius
    }

    /// Width of the hairline rim on every Pane's edge. Pane content stays inside it.
    pub(crate) const fn pane_rim_width(self) -> Pixels {
        px(PANE_RIM_WIDTH)
    }

    /// Resolves the terminal's paint interior against the rim's painted edges and inner arc. The
    /// terminal's logical grid insets stay independent of the rim.
    pub(crate) fn pane_paint_interior(
        self,
        bounds: Bounds<Pixels>,
        scale_factor: f32,
    ) -> PanePaintInterior {
        // Match GPUI Window::snap_bounds and snap_border_widths: round edges independently,
        // halves toward zero, and keep a nonzero stroke at least one device pixel wide. Radii
        // are scaled without snapping. WGPU's circular inner SDF subtracts the snapped border
        // from that outer radius, so subtract in device space before returning logical lengths.
        let snap = |logical: Pixels| {
            let device = f32::from(logical) * scale_factor;
            (device.abs() - 0.5).ceil().copysign(device)
        };
        let left = snap(bounds.left());
        let top = snap(bounds.top());
        let right = snap(bounds.right()).max(left);
        let bottom = snap(bounds.bottom()).max(top);
        let rim = snap(self.pane_rim_width()).max(1.0);
        let inner_left = left + rim;
        let inner_right = (right - rim).max(inner_left);
        let inner_bottom = (bottom - rim).max(top);
        let logical = |device: f32| px(device / scale_factor);
        PanePaintInterior {
            bounds: Bounds::from_corners(
                point(logical(inner_left), logical(top)),
                point(logical(inner_right), logical(inner_bottom)),
            ),
            corner_radius: logical((f32::from(self.pane_radius) * scale_factor - rim).max(0.0)),
        }
    }

    /// Corner radius of every selection chip: selected sidebar rows and the Active Tab.
    pub(crate) const fn chip_radius(self) -> Pixels {
        self.chip_radius
    }

    /// The offset a chip strip takes so its first chip paints on the strip's own leading edge.
    pub(crate) fn chip_strip_leading_offset(self) -> Pixels {
        px(0.0) - self.strip_chip_leading_inset
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ChromeDensity;
    use crate::ui::appearance::ChromeAppearance;

    const STANDARD_WINDOW_RADIUS: f32 = 16.0;
    const STANDARD_WINDOW_EDGE: f32 = 1.0;

    fn window(radius: Option<f32>) -> WindowFrameGeometry {
        WindowFrameGeometry::new(radius)
    }

    fn standard_window() -> WindowFrameGeometry {
        window(Some(STANDARD_WINDOW_RADIUS)).with_outer_edge_width(STANDARD_WINDOW_EDGE)
    }

    fn frame(density: ChromeDensity, window: WindowFrameGeometry) -> WorkspaceFrame {
        WorkspaceFrame::resolve(ChromeAppearance::density_spacing_scale(density), window)
    }

    #[test]
    fn every_density_should_resolve_whole_point_metrics() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            for window in [standard_window(), window(None)] {
                let frame = frame(density, window);
                for metric in [
                    frame.space(),
                    frame.window_edge_inset(),
                    frame.strip_chip_leading_inset(),
                    frame.strip_chip_trailing_inset(),
                    frame.pane_gap(),
                    frame.sidebar_chip_leading_inset(),
                    frame.sidebar_chip_trailing_inset(),
                    frame.pane_radius(),
                    frame.chip_radius(),
                ] {
                    let value = f32::from(metric);
                    assert_eq!(value, value.round(), "{density:?} {window:?} {frame:?}");
                    assert!(value > 0.0);
                }
            }
        }
    }

    #[test]
    fn comfortable_density_should_not_shrink_the_frame() {
        let compact = frame(ChromeDensity::Compact, standard_window());
        let comfortable = frame(ChromeDensity::Comfortable, standard_window());
        assert!(comfortable.space() > compact.space());
        assert_eq!((compact.space(), comfortable.space()), (px(5.0), px(6.0)));
    }

    /// Every visible gap in the Workspace is the same measurement, not a sum that agrees. The
    /// Split gap alone gives back two points so it looks equal.
    #[test]
    fn every_visible_gap_should_be_one_measurement() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, standard_window());
            let space = frame.space();
            let edge = frame.window_edge();
            assert_eq!(edge, px(STANDARD_WINDOW_EDGE));
            assert_eq!(
                (
                    frame.pane_gap() + px(SPLIT_GAP_OPTICAL_TRIM),
                    frame.sidebar_chip_trailing_inset(),
                    frame.sidebar_chip_leading_inset() - edge,
                    frame.stage_leading_inset(false) - edge,
                    frame.window_edge_inset() - edge,
                    frame.strip_chip_leading_inset() + frame.strip_chip_trailing_inset(),
                ),
                (space, space, space, space, space, space),
                "{density:?} gaps should be one measurement"
            );
            assert_eq!(
                frame.stage_leading_inset(true),
                px(0.0),
                "{density:?} the sidebar chip's own margin is the whole gap beside the Pane"
            );
        }
    }

    /// A top chip keeps one gap below the window edge and one above the Pane, at its own height.
    #[test]
    fn top_chrome_should_hold_its_chip_between_two_visible_gaps() {
        let base = px(36.0);
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, standard_window());
            assert_eq!(
                frame.top_chrome_height(base),
                frame.window_edge() + frame.space() + frame.top_chip_height(base) + frame.space(),
                "{density:?} a top chip should sit between two visible gaps"
            );
        }
        assert_eq!(
            frame(ChromeDensity::Compact, standard_window()).top_chip_height(base),
            px(30.0),
            "the gap never resizes a top chip"
        );
    }

    /// Two neighbouring chips each contribute a share of one visible gap.
    #[test]
    fn paired_chips_should_sum_to_one_visible_gap() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, standard_window());
            assert_eq!(
                frame.strip_chip_trailing_inset() + frame.strip_chip_leading_inset(),
                frame.space()
            );
            // A chip's paint starts on its strip's leading edge, so the control before the strip
            // supplies the whole gap by itself.
            assert_eq!(
                frame.chip_strip_leading_offset() + frame.strip_chip_leading_inset(),
                px(0.0),
                "{density:?} a first chip should paint on the strip edge"
            );
        }
    }

    /// Pane corners stay concentric with the native window while selection uses one fixed role.
    #[test]
    fn pane_radius_follows_its_inset_while_selection_radius_stays_fixed() {
        let compact = frame(ChromeDensity::Compact, standard_window());
        let comfortable = frame(ChromeDensity::Comfortable, standard_window());
        for (density, frame) in [
            (ChromeDensity::Compact, compact),
            (ChromeDensity::Comfortable, comfortable),
        ] {
            assert_eq!(
                frame.pane_radius() + frame.window_edge_inset(),
                px(STANDARD_WINDOW_RADIUS),
                "{density:?} Pane corners should stay concentric with the window corner"
            );
            assert!(
                frame.chip_radius() <= frame.pane_radius(),
                "{density:?} a chip is the smallest shape in the family, got {frame:?}"
            );
        }
        assert_eq!(
            (compact.chip_radius(), comfortable.chip_radius()),
            (RadiusRole::Control.pixels(), RadiusRole::Control.pixels())
        );
        assert_eq!(
            (compact.pane_radius(), comfortable.pane_radius()),
            (px(10.0), px(9.0)),
            "the Pane radius must derive from each density's actual inset"
        );
    }

    #[test]
    fn invalid_or_missing_window_facts_should_use_the_portable_fallback() {
        let unavailable = frame(ChromeDensity::Compact, window(None));
        for radius in [f32::NAN, -1.0] {
            assert_eq!(
                frame(ChromeDensity::Compact, window(Some(radius))),
                unavailable
            );
        }
        for edge in [f32::NAN, -1.0] {
            assert_eq!(
                frame(
                    ChromeDensity::Compact,
                    window(None).with_outer_edge_width(edge)
                ),
                unavailable
            );
        }
        assert_eq!(unavailable.window_edge(), px(0.0));
        assert_eq!(
            frame(ChromeDensity::Compact, window(Some(0.0))).pane_radius(),
            RadiusRole::Control.pixels()
        );
    }

    #[test]
    fn pane_radius_should_not_exceed_the_largest_semantic_surface() {
        assert_eq!(
            frame(ChromeDensity::Compact, window(Some(100.0))).pane_radius(),
            RadiusRole::SurfaceLarge.pixels()
        );
    }

    #[test]
    fn invalid_spacing_scale_should_resolve_as_compact() {
        let window = window(None);
        assert_eq!(
            WorkspaceFrame::resolve(f32::NAN, window),
            frame(ChromeDensity::Compact, window)
        );
    }
}
