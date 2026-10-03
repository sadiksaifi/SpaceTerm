use crate::ui::appearance::gpui_color;
use std::time::Duration;

use gpui::px;
use spaceterm_ui::{
    FieldFrameTheme, TextInputMetrics, TextInputPaint, TextInputTheme, TextInputVariants,
};

use crate::appearance::ChromeColors;
use crate::ui::chrome_geometry::HAIRLINE;

pub(super) fn theme(colors: &ChromeColors) -> TextInputTheme {
    themed(colors, colors)
}

/// Standard fields paint a frame compiled for their host; Bare fields inherit the host directly.
pub(super) fn themed(standard: &ChromeColors, bare: &ChromeColors) -> TextInputTheme {
    let paint = |colors: &ChromeColors| {
        TextInputPaint::new(
            gpui_color(colors.input_text),
            gpui_color(colors.input_placeholder),
            gpui_color(colors.input_selection_background),
            gpui_color(colors.input_caret),
            gpui_color(colors.input_disabled_text),
            gpui_color(colors.input_disabled_text),
        )
        .selection_foreground(gpui_color(colors.input_selection_foreground))
    };
    TextInputTheme::new(
        TextInputVariants::new(paint(standard), paint(bare)),
        TextInputMetrics::new(px(HAIRLINE), px(2.0), Duration::from_millis(16), px(24.0)),
    )
    .field_frame(FieldFrameTheme::new(
        gpui_color(standard.input_background),
        gpui_color(standard.input_border),
        gpui_color(standard.input_invalid_border),
        gpui_color(standard.input_disabled_background),
        gpui_color(standard.input_disabled_border),
        gpui_color(standard.focus_ring),
    ))
}
