//! Product-owned Chrome radii and structural edge width, in GPUI logical points.
//! Density changes spacing and control heights, but leaves these values unchanged. The Pane is the
//! single named exception: its radius follows the native outer corner after the density-scaled
//! frame inset, bounded by the Control and SurfaceLarge roles.

use gpui::{Pixels, px};

/// The structural edge: separators, control borders, rims, and focus rings.
pub(crate) const HAIRLINE: f32 = 1.0;

/// Small controls: 20 pt actions, chips, badges, swatches, and inline marks.
pub(crate) const RADIUS_CONTROL_SMALL: f32 = 4.0;
/// Ordinary controls: fields, buttons, pop-up triggers, Tabs, and list rows.
pub(crate) const RADIUS_CONTROL: f32 = 6.0;
/// Grouped content: Settings cards and the runs of rows they hold.
pub(crate) const RADIUS_CARD: f32 = 8.0;
/// Transient surfaces: popovers, menus, tooltips, and Pane notices.
pub(crate) const RADIUS_SURFACE: f32 = 10.0;
/// Large transient surfaces: command surfaces, dialogs, and the Settings Window.
pub(crate) const RADIUS_SURFACE_LARGE: f32 = 12.0;

/// A named radius, so a caller states what a surface is rather than how round it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RadiusRole {
    ControlSmall,
    Control,
    Card,
    Surface,
    SurfaceLarge,
}

impl RadiusRole {
    pub(crate) const fn points(self) -> f32 {
        match self {
            Self::ControlSmall => RADIUS_CONTROL_SMALL,
            Self::Control => RADIUS_CONTROL,
            Self::Card => RADIUS_CARD,
            Self::Surface => RADIUS_SURFACE,
            Self::SurfaceLarge => RADIUS_SURFACE_LARGE,
        }
    }

    pub(crate) fn pixels(self) -> Pixels {
        px(self.points())
    }
}

/// Preserves concentric corners for an outline outside its reference surface.
pub(crate) const fn concentric_outset(inner: f32, outset: f32) -> f32 {
    inner + outset
}

/// Carries the native window corner inward by the frame inset without escaping the radius scale.
pub(crate) fn pane_radius(outer_radius: f32, frame_inset: f32) -> f32 {
    (outer_radius - frame_inset)
        .round()
        .clamp(RADIUS_CONTROL, RADIUS_SURFACE_LARGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outer_outlines_keep_concentric_corners() {
        assert_eq!(concentric_outset(RADIUS_CONTROL, 3.0), 9.0);
    }

    #[test]
    fn pane_radius_is_the_only_native_derived_radius_and_stays_inside_the_scale() {
        assert_eq!(pane_radius(0.0, 6.0), RADIUS_CONTROL);
        assert_eq!(pane_radius(16.0, 6.0), 10.0);
        assert_eq!(pane_radius(100.0, 6.0), RADIUS_SURFACE_LARGE);
    }
}
