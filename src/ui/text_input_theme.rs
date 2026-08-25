use std::time::Duration;

use gpui::{Rgba, px, rgba};
use spaceterm_ui::{TextInputMetrics, TextInputPaint, TextInputTheme, TextInputVariants};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> TextInputTheme {
    let paint = TextInputPaint::new(
        gpui_color(ACTIVE_THEME.text),
        gpui_color(ACTIVE_THEME.text_placeholder),
        gpui_color(ACTIVE_THEME.players[0].selection),
        gpui_color(ACTIVE_THEME.players[0].cursor),
        gpui_color(ACTIVE_THEME.text_disabled),
        gpui_color(ACTIVE_THEME.text_disabled),
    );
    TextInputTheme::new(
        TextInputVariants::new(paint, paint),
        TextInputMetrics::new(px(1.0), px(2.0), Duration::from_millis(16), px(24.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
