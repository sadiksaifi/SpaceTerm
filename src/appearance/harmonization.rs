//! Theme Harmonization: fits a Terminal Theme's background to the Application Chrome's Pane band.
//!
//! The Application Chrome stays SpaceTerm's. A theme keeps its hue, text colors, and palette, but
//! its background rests where SpaceTerm's own Panes rest: one quiet step above the Chrome root.

use std::ops::RangeInclusive;

use super::{Appearance, TerminalColors, builtin};

/// Where a Pane background may rest, in OKLab lightness above the Chrome root, and how much
/// chroma it may keep. SpaceTerm's own Terminal Theme backgrounds lie inside both bands.
struct PaneBand {
    above_root: RangeInclusive<f64>,
    max_lightness: f64,
    max_chroma: f64,
}

const DARK: PaneBand = PaneBand {
    above_root: 0.008..=0.05,
    max_lightness: 1.0,
    max_chroma: 0.035,
};

/// Light Panes may reach near-white, but pure white costs ink that ignores the Opacity Setting.
const LIGHT: PaneBand = PaneBand {
    above_root: 0.033..=1.0,
    max_lightness: 0.994,
    max_chroma: 0.03,
};

/// Fits a Terminal Theme's background to the Application Chrome's Pane band. A background
/// already inside the band changes nothing.
pub(super) fn harmonize(colors: &mut TerminalColors, appearance: Appearance) {
    let band = match appearance {
        Appearance::Dark => DARK,
        Appearance::Light => LIGHT,
    };
    let root = builtin::chrome_definition(appearance)
        .background
        .oklab_lightness();
    let lightness =
        root + band.above_root.start()..=(root + band.above_root.end()).min(band.max_lightness);
    colors.background = colors.background.fit_oklab(lightness, band.max_chroma);
}
