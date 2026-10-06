//! Theme Harmonization: fits a Terminal Theme's background to the Application Chrome's Pane band.
//!
//! The Application Chrome stays SpaceTerm's. A theme keeps its hue, text colors, and palette, but
//! its background rests where SpaceTerm's own Panes rest: one quiet step above the Chrome root.

use std::ops::RangeInclusive;

use super::{Appearance, Color, TerminalColors, builtin};

/// Where a Pane background may rest, in OKLab lightness above the Chrome root, and how much
/// chroma it may keep. SpaceTerm's own Terminal Theme backgrounds lie inside both bands.
///
/// A translucent window rebuilds a tinted Pane over its sheet with saturated ink, so a chroma
/// that reads as a quiet tint when opaque reads as a colored slab against the Chrome. Only a
/// trace of the theme's hue survives.
struct PaneBand {
    above_root: RangeInclusive<f64>,
    max_lightness: f64,
    max_chroma: f64,
}

const DARK: PaneBand = PaneBand {
    above_root: 0.008..=0.03,
    max_lightness: 1.0,
    max_chroma: 0.006,
};

/// Light Panes may reach near-white, but pure white costs ink that ignores the Opacity Setting.
const LIGHT: PaneBand = PaneBand {
    above_root: 0.033..=1.0,
    max_lightness: 0.994,
    max_chroma: 0.006,
};

/// The contrast a text color keeps against a moved background, unless its author gave it less.
/// Intentionally quiet colors, such as a gray ANSI black, stay quiet.
const TEXT_CONTRAST: f64 = 4.5;

/// Fits a Terminal Theme's background to the Application Chrome's Pane band and restores the
/// contrast its opaque text colors lose. A background already inside the band changes nothing.
/// Translucent selection, search, and bell overlays stay as authored.
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
    let authored = colors.background;
    let background = authored.fit_oklab(lightness, band.max_chroma);
    if background == authored {
        return;
    }
    colors.background = background;
    let restore = |color: &mut Color| {
        let required = color.contrast_ratio(authored).min(TEXT_CONTRAST);
        if color.contrast_ratio(background) >= required {
            return;
        }
        // Keep the side of the background the author chose. When that side has no room left,
        // readability wins and the color crosses to the nearer readable side.
        let lighter = color.oklab_lightness() > authored.oklab_lightness();
        if let Some(readable) = color
            .readable_preserving_chroma_toward(&[background], required, lighter)
            .or_else(|| color.readable_preserving_chroma(&[background], required))
        {
            *color = readable;
        }
    };
    for color in [
        &mut colors.foreground,
        &mut colors.bright_foreground,
        &mut colors.dim_foreground,
        &mut colors.cursor,
        &mut colors.hyperlink,
    ]
    .into_iter()
    .chain(&mut colors.normal)
    .chain(&mut colors.bright)
    .chain(&mut colors.dim)
    .chain(colors.cursor_text.as_mut())
    .chain(colors.selection_foreground.as_mut())
    .chain(colors.find_match_foreground.as_mut())
    .chain(colors.find_active_match_foreground.as_mut())
    {
        restore(color);
    }
}
