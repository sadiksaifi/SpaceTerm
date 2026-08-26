use gpui::{Rgba, px, rgba};
use spaceterm_ui::{TooltipMetrics, TooltipPaint, TooltipTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> TooltipTheme {
    TooltipTheme::new(
        TooltipPaint::new(
            gpui_color(ACTIVE_THEME.elevated_surface_background),
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.text_muted),
            gpui_color(ACTIVE_THEME.text_muted),
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
