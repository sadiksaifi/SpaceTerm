//! Geometry of the floating Workspace frame: the one negative-space measurement the composition
//! breathes with, fixed selection geometry, the Pane corner derived from its native container, and
//! the top chrome height that absorbs the frame's top edge.
//!
//! Every visible gap in the Workspace is that one measurement: the Pane stage's perimeter, the
//! space between Split Panes at any nesting depth, and the margin on both sides of a sidebar item's
//! chip. They are literally equal rather than sums that happen to agree.
//!
//! The frame has no top edge to paint. The space that would sit between the titlebar and the Pane
//! belongs to the top chrome's height instead, so the Tab strip breathes with the same measurement
//! and the Pane starts at the chrome's lower edge.
//!
//! These are layout metrics, not color roles. Every consumer reads one resolved [`WorkspaceFrame`]
//! so the space, fixed selection radius, derived Pane radius, and chrome height cannot drift apart
//! across WorkspaceManager, TabManager, WorkspaceSidebar, and PaneHost.

use gpui::{App, Pixels, px};

use crate::appearance::{ChromeColors, Color};
use crate::platform::window_frame::WindowFrameGeometry;
use crate::ui::chrome_geometry::RadiusRole;

/// The one continuous Chrome surface beneath the Workspace: the sidebar region, the content stage
/// insets, and the gaps between Panes.
///
/// This selects an existing Chrome role rather than introducing one, so the sidebar, the stage, and
/// each Pane's corner mask cannot disagree about the surface they share.
pub(crate) fn base_surface(colors: &ChromeColors) -> Color {
    colors.panel_background
}

/// The Compact-density measurement of every visible gap in the Workspace.
const SPACE: f32 = RadiusRole::Control.points();
/// The smallest Pane radius that still reads as a rounded surface at every density.
const MINIMUM_PANE_RADIUS: f32 = RadiusRole::Control.points();

/// Used when the hosting platform cannot supply an outer window radius.
const FALLBACK_WINDOW_RADIUS: f32 = 12.0;

/// Resolved whole-point geometry of the floating Workspace frame for one density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkspaceFrame {
    space: Pixels,
    half_space: Pixels,
    pane_radius: Pixels,
    chip_radius: Pixels,
}

impl WorkspaceFrame {
    /// Resolves the frame for a Chrome density scale and the hosting window's corner.
    ///
    /// Each length is rounded to a whole point so Comfortable density never leaves a fractional,
    /// blurry edge between a Pane and the base surface.
    pub(crate) fn resolve(spacing_scale: f32, window: WindowFrameGeometry) -> Self {
        let scale = if spacing_scale.is_finite() && spacing_scale > 0.0 {
            spacing_scale
        } else {
            1.0
        };
        let space = (SPACE * scale).round();
        // Selection is a fixed semantic control radius. A Pane is the native window corner carried
        // inward by the density-scaled frame space, so its radius follows the changing inset
        // instead of selecting an independent density-scaled value.
        let chip_radius = RadiusRole::Control.points();
        let window_radius = match window.outer_corner_radius() {
            Some(radius) if radius.is_finite() && radius >= 0.0 => radius,
            Some(_) | None => FALLBACK_WINDOW_RADIUS,
        };
        let pane_radius = (window_radius - space).round().max(MINIMUM_PANE_RADIUS);
        Self {
            space: px(space),
            half_space: px((space / 2.0).round()),
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
    ///
    /// It is a distance between painted surfaces, not a layout property. Two insets of this size
    /// may never meet, because the base surface is continuous and adjacent insets would read as one
    /// gap of twice the size.
    pub(crate) const fn space(self) -> Pixels {
        self.space
    }

    /// What each of two neighbouring chips contributes to the gap between them.
    ///
    /// Chips in one strip are inset inside their own items, so both sides of a boundary contribute
    /// and the visible gap is [`WorkspaceFrame::space`] again.
    pub(crate) const fn half_space(self) -> Pixels {
        self.half_space
    }

    /// Base surface between two Split Panes; it also owns the Split's resize hit target.
    pub(crate) const fn pane_gap(self) -> Pixels {
        self.space
    }

    /// The margin a sidebar item's chip keeps on both of its sides.
    ///
    /// A row's chip faces the window edge on one side and the Pane beside the sidebar on the other.
    /// Both neighbours are painted surfaces rather than chips, so the chip carries the whole gap on
    /// each side and the stage adds nothing where they meet.
    pub(crate) const fn sidebar_chip_inset(self) -> Pixels {
        self.space
    }

    /// The Pane stage's leading perimeter.
    ///
    /// With a sidebar present the visible gap is already painted by the sidebar chip's own trailing
    /// margin, so the stage adds nothing: two insets of the same base surface would otherwise merge
    /// into one gap of twice the size. Without a sidebar the stage carries the window-edge
    /// perimeter itself.
    pub(crate) fn stage_leading_inset(self, sidebar_visible: bool) -> Pixels {
        if sidebar_visible { px(0.0) } else { self.space }
    }

    /// The height of the top chrome, which absorbs the frame's top space.
    ///
    /// Nothing is painted between the titlebar and the Pane: the Tab strip simply grows by the
    /// frame's measurement, so its chips keep the same air above and below that every other gap
    /// in the Workspace has.
    pub(crate) fn top_chrome_height(self, base_height: Pixels) -> Pixels {
        base_height + self.space
    }

    /// Corner radius of every floating Pane surface.
    pub(crate) const fn pane_radius(self) -> Pixels {
        self.pane_radius
    }

    /// Corner radius of every selection chip: selected sidebar rows and the Active Tab.
    pub(crate) const fn chip_radius(self) -> Pixels {
        self.chip_radius
    }

    /// The offset a chip strip takes so its first chip paints on the strip's own leading edge.
    ///
    /// A chip is inset inside its item, which would add to the gap the control before the strip
    /// already leaves. The strip pulls that inset back instead, so the visible distance from the
    /// preceding control to the first chip is one [`WorkspaceFrame::space`], and the first Tab's
    /// paint lines up with the Pane beneath it.
    pub(crate) fn chip_strip_leading_offset(self) -> Pixels {
        px(0.0) - self.half_space
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::ChromeDensity;
    use crate::ui::appearance::ChromeAppearance;

    const STANDARD_WINDOW_RADIUS: f32 = 16.0;

    fn window(radius: Option<f32>) -> WindowFrameGeometry {
        WindowFrameGeometry::new(radius)
    }

    fn frame(density: ChromeDensity, window: WindowFrameGeometry) -> WorkspaceFrame {
        WorkspaceFrame::resolve(ChromeAppearance::density_spacing_scale(density), window)
    }

    #[test]
    fn every_density_should_resolve_whole_point_metrics() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            for window in [window(Some(STANDARD_WINDOW_RADIUS)), window(None)] {
                let frame = frame(density, window);
                for metric in [
                    frame.space(),
                    frame.half_space(),
                    frame.pane_gap(),
                    frame.sidebar_chip_inset(),
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
        let window = window(Some(STANDARD_WINDOW_RADIUS));
        let compact = frame(ChromeDensity::Compact, window);
        let comfortable = frame(ChromeDensity::Comfortable, window);
        assert!(comfortable.space() > compact.space());
        assert_eq!((compact.space(), comfortable.space()), (px(6.0), px(8.0)));
    }

    /// Every visible gap in the Workspace is the same measurement, not a sum that agrees.
    #[test]
    fn every_visible_gap_should_be_one_measurement() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, window(Some(STANDARD_WINDOW_RADIUS)));
            let space = frame.space();
            assert_eq!(
                (
                    frame.pane_gap(),
                    frame.sidebar_chip_inset(),
                    frame.stage_leading_inset(false),
                    frame.top_chrome_height(px(36.0)) - px(36.0),
                    frame.half_space() * 2.0,
                ),
                (space, space, space, space, space),
                "{density:?} gaps should be one measurement"
            );
            assert_eq!(
                frame.stage_leading_inset(true),
                px(0.0),
                "{density:?} the sidebar chip's own margin is the whole gap beside the Pane"
            );
            assert_eq!(
                frame.chip_strip_leading_offset(),
                px(0.0) - frame.half_space(),
                "{density:?} a strip pulls its first chip's inset back"
            );
        }
    }

    /// Two neighbouring chips each contribute half of one visible gap.
    #[test]
    fn paired_chips_should_sum_to_one_visible_gap() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, window(Some(STANDARD_WINDOW_RADIUS)));
            assert_eq!(frame.half_space() + frame.half_space(), frame.space());
            // A chip's paint starts on its strip's leading edge, so the control before the strip
            // supplies the whole gap by itself.
            assert_eq!(
                frame.chip_strip_leading_offset() + frame.half_space(),
                px(0.0),
                "{density:?} a first chip should paint on the strip edge"
            );
        }
    }

    /// Pane corners stay concentric with the native window while selection uses one fixed role.
    #[test]
    fn pane_radius_follows_its_inset_while_selection_radius_stays_fixed() {
        let window = window(Some(STANDARD_WINDOW_RADIUS));
        let compact = frame(ChromeDensity::Compact, window);
        let comfortable = frame(ChromeDensity::Comfortable, window);
        for (density, frame) in [
            (ChromeDensity::Compact, compact),
            (ChromeDensity::Comfortable, comfortable),
        ] {
            assert_eq!(
                frame.pane_radius() + frame.space(),
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
            (px(10.0), px(8.0)),
            "the Pane radius must derive from each density's actual inset"
        );
    }

    #[test]
    fn invalid_or_missing_corner_facts_should_use_the_portable_fallback() {
        let unavailable = frame(ChromeDensity::Compact, window(None));
        for radius in [f32::NAN, -1.0] {
            assert_eq!(
                frame(ChromeDensity::Compact, window(Some(radius))),
                unavailable
            );
        }
        assert_eq!(
            frame(ChromeDensity::Compact, window(Some(0.0))).pane_radius(),
            px(MINIMUM_PANE_RADIUS)
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
