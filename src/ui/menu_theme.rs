use gpui::{Rgba, px, rgba};
use spaceterm_ui::{MenuMetrics, MenuPaint, MenuSizes, MenuTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> MenuTheme {
    let paint = MenuPaint::new(
        gpui_color(ACTIVE_THEME.elevated_surface_background),
        gpui_color(ACTIVE_THEME.border),
        gpui_color(ACTIVE_THEME.text),
        gpui_color(ACTIVE_THEME.icon),
        gpui_color(ACTIVE_THEME.text_disabled),
        gpui_color(ACTIVE_THEME.element_hover),
        gpui_color(ACTIVE_THEME.text),
        gpui_color(ACTIVE_THEME.error),
        gpui_color(ACTIVE_THEME.border),
    )
    .trigger(
        gpui_color(ACTIVE_THEME.ghost_element_background),
        gpui_color(ACTIVE_THEME.ghost_element_hover),
        gpui_color(ACTIVE_THEME.border_transparent),
    )
    .focus_border(gpui_color(ACTIVE_THEME.border_focused));

    MenuTheme::new(
        paint,
        MenuSizes::new(metrics(196.0), metrics(208.0), metrics(240.0)),
    )
}

fn metrics(width: f32) -> MenuMetrics {
    MenuMetrics::new(px(width), px(26.0))
        .trigger_height(px(28.0))
        .horizontal_padding(px(6.0))
        .indicator_width(px(16.0))
        .gap(px(6.0))
        .corner_radius(px(8.0))
        .border_width(px(1.0))
        .font_sizes(px(12.0), px(11.0))
        .panel_spacing(px(3.0), px(2.0))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
