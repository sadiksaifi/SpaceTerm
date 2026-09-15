//! Geometry of the floating Workspace frame: the base-surface inset around the Pane Layout, the
//! gap between Split Panes, and the corner radius of each floating Pane surface.
//!
//! These are layout metrics, not color roles. Every consumer reads one resolved [`WorkspaceFrame`]
//! so the inset, gap, and radius cannot drift apart across WorkspaceManager, TabManager, and
//! PaneHost.

use gpui::{App, Pixels, px};

use crate::appearance::{ChromeColors, Color};
use crate::platform::window_frame::WindowFrameGeometry;

/// The one continuous Chrome surface beneath the Workspace: the sidebar region, the content stage
/// insets, and the gaps between Panes.
///
/// This selects an existing Chrome role rather than introducing one, so the sidebar, the stage, and
/// each Pane's corner mask cannot disagree about the surface they share.
pub(crate) fn base_surface(colors: &ChromeColors) -> Color {
    colors.panel_background
}

/// The Compact-density base-surface inset between the content stage edges and a Pane.
///
/// It is the selection chip's radius, so the frame's negative space and the smallest shape in the
/// Chrome hierarchy are one measurement rather than two that happen to agree.
const OUTER_INSET: f32 = super::selection_chip::CHIP_RADIUS;
/// The Compact-density empty base surface between two Split Panes. It matches the inset so a
/// Split reads with the same rhythm as the frame around it.
const PANE_GAP: f32 = OUTER_INSET;
/// The smallest Pane radius that still reads as a rounded surface at every density.
const MINIMUM_PANE_RADIUS: f32 = super::selection_chip::CHIP_RADIUS;
/// The Compact-density air a navigation chip keeps to the edge of the surface it rests on.
///
/// It is the hairline gap the keyboard focus ring needs outside a chip, so a ring never has to
/// paint outside the strip that owns it.
const EDGE_RESERVE: f32 = 2.0;

/// Used when the hosting platform cannot supply an outer window radius.
const FALLBACK_WINDOW_RADIUS: f32 = 12.0;

/// Resolved whole-point geometry of the floating Workspace frame for one density.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct WorkspaceFrame {
    outer_inset: Pixels,
    pane_gap: Pixels,
    pane_radius: Pixels,
    chip_radius: Pixels,
    edge_reserve: Pixels,
}

/// Where a navigation strip's selection chip sits inside the row that owns it.
///
/// A sidebar row rests against the window edge on one side and against the floating content stage
/// on the other, so its chip needs different insets to keep the same air on both sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ChipInsets {
    pub(crate) leading: Pixels,
    pub(crate) trailing: Pixels,
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
        let outer_inset = (OUTER_INSET * scale).round();
        let pane_gap = (PANE_GAP * scale).round();
        // One radius family: the chip is the smallest shape, and a Pane is the window's own corner
        // carried inward by the frame inset, so both grow from the same two facts.
        let chip_radius = (super::selection_chip::CHIP_RADIUS * scale).round();
        let window_radius = match window.outer_corner_radius() {
            Some(radius) if radius.is_finite() && radius >= 0.0 => radius,
            Some(_) | None => FALLBACK_WINDOW_RADIUS,
        };
        let pane_radius = (window_radius - outer_inset)
            .round()
            .max(chip_radius.min(MINIMUM_PANE_RADIUS));
        Self {
            outer_inset: px(outer_inset),
            pane_gap: px(pane_gap),
            pane_radius: px(pane_radius),
            chip_radius: px(chip_radius),
            edge_reserve: px((EDGE_RESERVE * scale).round()),
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

    /// Base surface between the content stage edges and a Pane on all four sides.
    pub(crate) const fn outer_inset(self) -> Pixels {
        self.outer_inset
    }

    /// Base surface between two Split Panes; it also owns the Split's resize hit target.
    pub(crate) const fn pane_gap(self) -> Pixels {
        self.pane_gap
    }

    /// Corner radius of every floating Pane surface.
    pub(crate) const fn pane_radius(self) -> Pixels {
        self.pane_radius
    }

    /// Corner radius of every selection chip: selected sidebar rows and the Active Tab.
    pub(crate) const fn chip_radius(self) -> Pixels {
        self.chip_radius
    }

    /// The air a navigation chip keeps to the edge of the strip that owns it.
    pub(crate) const fn edge_reserve(self) -> Pixels {
        self.edge_reserve
    }

    /// Chip insets for a strip that meets the window edge on its leading side and the floating
    /// content stage on its trailing side, so both visible margins come out equal.
    ///
    /// The trailing margin is completed by the stage's own inset, which is why the trailing inset
    /// is the smaller of the two.
    pub(crate) fn stage_adjacent_chip_insets(self) -> ChipInsets {
        ChipInsets {
            leading: self.outer_inset + self.edge_reserve,
            trailing: self.edge_reserve,
        }
    }

    /// The leading padding a strip needs so its first chip lines up with the content stage.
    ///
    /// The strip starts where the stage starts, but a chip is inset inside its own item, so the
    /// strip gives back exactly the difference.
    pub(crate) fn chip_alignment_padding(self, chip_inset: Pixels) -> Pixels {
        (self.outer_inset - chip_inset).max(px(0.0))
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
                    frame.outer_inset(),
                    frame.pane_gap(),
                    frame.pane_radius(),
                    frame.chip_radius(),
                    frame.edge_reserve(),
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
        assert!(comfortable.outer_inset() >= compact.outer_inset());
        assert!(comfortable.pane_gap() >= compact.pane_gap());
    }

    /// Pane corners and selection chips are one family: every radius grows from the chip baseline
    /// and the window corner, so no surface can drift into a shape of its own.
    #[test]
    fn pane_and_chip_radii_should_stay_in_one_family_at_every_density() {
        let window = window(Some(STANDARD_WINDOW_RADIUS));
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, window);
            assert_eq!(
                frame.pane_radius() + frame.outer_inset(),
                px(STANDARD_WINDOW_RADIUS),
                "{density:?} Pane corners should stay concentric with the window corner"
            );
            assert!(
                frame.chip_radius() <= frame.pane_radius(),
                "{density:?} a chip is the smallest shape in the family, got {frame:?}"
            );
            assert_eq!(frame.outer_inset(), frame.chip_radius());
        }
    }

    /// A sidebar row rests against the window edge on one side and the floating stage on the other.
    /// Its chip keeps the same visible air on both, once the stage's own inset is counted.
    #[test]
    fn stage_adjacent_chips_should_keep_equal_visible_margins() {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let frame = frame(density, window(Some(STANDARD_WINDOW_RADIUS)));
            let insets = frame.stage_adjacent_chip_insets();
            assert_eq!(
                insets.leading,
                insets.trailing + frame.outer_inset(),
                "{density:?} leading air should equal trailing air plus the stage inset"
            );
            assert!(insets.trailing >= frame.edge_reserve());
        }
    }

    #[test]
    fn chip_alignment_padding_should_land_a_first_chip_on_the_stage_edge() {
        let frame = frame(ChromeDensity::Compact, window(Some(STANDARD_WINDOW_RADIUS)));
        assert_eq!(
            frame.chip_alignment_padding(px(2.0)) + px(2.0),
            frame.outer_inset()
        );
        assert_eq!(frame.chip_alignment_padding(px(40.0)), px(0.0));
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
