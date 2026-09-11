use gpui::{Rgba, px, rgba};
use spaceterm_ui::{TooltipMetrics, TooltipPaint, TooltipTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> TooltipTheme {
    TooltipTheme::new(
        TooltipPaint::new(
            gpui_color(colors.elevated_surface_background),
            gpui_color(colors.border),
            gpui_color(colors.text),
            gpui_color(colors.text_muted),
            gpui_color(colors.text_muted),
        ),
        TooltipMetrics::new(px(480.0))
            .spacing(px(8.0), px(5.0), px(3.0), px(12.0), px(6.0), px(8.0))
            .surface(px(5.0), px(1.0))
            .font_sizes(px(11.0), px(10.0), px(10.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
