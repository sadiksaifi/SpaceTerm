// Production progress presentation is selected here so reusable controls remain independent of
// the application's appearance vocabulary.

use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ProgressMetrics, ProgressMotion, ProgressPaint, ProgressSizes, ProgressTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors, motion: ProgressMotion) -> ProgressTheme {
    ProgressTheme::new(
        paint(colors),
        ProgressSizes::new(
            // Compact belongs in status rows and pane captions: a hairline-weight bar beside
            // caption text, and a spinner that fits inside a caption's line box.
            ProgressMetrics::new(px(3.0), px(1.5), px(12.0), px(1.5)),
            // Regular belongs in panels, sheets, and modal content. It stays restrained: the bar
            // is a thin capsule rather than a filled band, and the ring stays a small indicator
            // beside body text rather than a graphic the surface is built around.
            ProgressMetrics::new(px(4.0), px(2.0), px(18.0), px(2.0)),
        ),
        motion,
    )
}

fn paint(colors: &ChromeColors) -> ProgressPaint {
    // The track takes the canonical unswitched mark track, the one neutral role each appearance
    // authors for a band an indicator rides along. Every scheme keeps it a visible step off the
    // window root and the chrome shell, so a zero extent still reads as a track and a partial
    // extent reads as progress along it, while the role itself stays achromatic and never competes
    // with the indicator. The derived element surfaces are tuned to sit flush with the surface
    // under them, which is right for a resting control and leaves a thin band invisible.
    //
    // The indicator is the canonical accent, which is what this palette reserves for small active
    // indicators and what modal progress already fills with, so one operation reads the same
    // wherever it is presented.
    ProgressPaint::new(
        gpui_color(colors.toggle_off_background),
        gpui_color(colors.text_accent),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
