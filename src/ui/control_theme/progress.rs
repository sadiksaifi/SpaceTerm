// Production progress presentation is selected here so reusable controls remain independent of
// the application's appearance vocabulary.

use crate::ui::appearance::gpui_color;
use gpui::px;
use spaceterm_ui::{ProgressMetrics, ProgressPaint, ProgressSizes, ProgressTheme};

use crate::appearance::ChromeColors;

pub(in crate::ui) fn theme(colors: &ChromeColors) -> ProgressTheme {
    ProgressTheme::new(
        paint(colors),
        ProgressSizes::new(
            // Compact belongs in status rows and pane captions: a hairline-weight bar beside
            // caption text, and a spinner that fits inside a caption's line box.
            ProgressMetrics::new(px(3.0), px(1.5), px(12.0), px(1.5)),
            ProgressMetrics::new(px(4.0), px(2.0), px(18.0), px(2.0)),
        ),
    )
}

fn paint(colors: &ChromeColors) -> ProgressPaint {
    ProgressPaint::new(
        gpui_color(colors.progress_track),
        gpui_color(colors.progress_indicator),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::Color;
    use gpui::rgba;

    #[test]
    fn paint_uses_dedicated_progress_roles() {
        let colors = ChromeColors {
            progress_track: Color::rgba(0x11223344),
            progress_indicator: Color::rgba(0x55667788),
            toggle_off_background: Color::rgb(0xaabbcc),
            text_accent: Color::rgb(0xddeeff),
            ..ChromeColors::default()
        };

        assert_eq!(
            paint(&colors),
            ProgressPaint::new(rgba(0x11223344), rgba(0x55667788))
        );
    }
}
