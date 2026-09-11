use std::time::Duration;

use gpui::{Rgba, px, rgba};
use spaceterm_ui::{TextInputMetrics, TextInputPaint, TextInputTheme, TextInputVariants};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> TextInputTheme {
    let paint = TextInputPaint::new(
        gpui_color(colors.input_text),
        gpui_color(colors.input_placeholder),
        gpui_color(colors.input_selection_background),
        gpui_color(colors.input_caret),
        gpui_color(colors.input_disabled_text),
        gpui_color(colors.input_disabled_text),
    );
    TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(24.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
