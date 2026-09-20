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
    ProgressPaint::new(
        gpui_color(colors.progress_track),
        gpui_color(colors.progress_indicator),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_uses_dedicated_progress_roles() {
        let mut colors = ChromeColors::default();
        colors.progress_track = Color::rgba(0x11223344);
        colors.progress_indicator = Color::rgba(0x55667788);
        colors.toggle_off_background = Color::rgb(0xaabbcc);
        colors.text_accent = Color::rgb(0xddeeff);

        assert_eq!(
            paint(&colors),
            ProgressPaint::new(rgba(0x11223344), rgba(0x55667788))
        );
    }
}
