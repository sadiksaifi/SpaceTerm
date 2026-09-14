use std::time::Duration;

use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    FieldFrameTheme, TextInputMetrics, TextInputPaint, TextInputTheme, TextInputVariants,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> TextInputTheme {
    let paint = TextInputPaint::new(
        gpui_color(colors.input_text),
        gpui_color(colors.input_placeholder),
        gpui_color(colors.input_selection_background),
        gpui_color(colors.input_caret),
        gpui_color(colors.input_disabled_text),
        gpui_color(colors.input_disabled_text),
    )
    .selection_foreground(gpui_color(colors.input_selection_foreground));
    TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(24.0)),
    )
    .field_frame(FieldFrameTheme::new(
        gpui_color(colors.input_background),
        gpui_color(colors.input_border),
        gpui_color(colors.input_focused_border),
        gpui_color(colors.input_invalid_border),
        gpui_color(colors.input_disabled_background),
        gpui_color(colors.input_disabled_border),
    ))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
