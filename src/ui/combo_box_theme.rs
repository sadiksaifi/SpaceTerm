use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ComboBoxTheme {
    ComboBoxTheme::new(
        ComboBoxPaint::new(
            gpui_color(colors.elevated_surface_background),
            gpui_color(colors.border),
            gpui_color(colors.text),
            gpui_color(colors.text_muted),
            gpui_color(colors.text_disabled),
            gpui_color(colors.element_selected),
            gpui_color(colors.element_selected_foreground),
            gpui_color(colors.ghost_element_background),
            gpui_color(colors.ghost_element_hover),
            gpui_color(colors.border_transparent),
            gpui_color(colors.border_focused),
        ),
        ComboBoxMetrics::new(px(240.0), px(40.0))
            .icon_trigger_size(px(28.0))
            .geometry(px(260.0), px(28.0), px(30.0), px(46.0))
            .spacing(px(4.0), px(10.0), px(18.0), px(8.0))
            .shape(px(7.0), px(1.0))
            .font_sizes(px(12.0), px(11.0)),
    )
    .shadow(super::appearance::control_shadow(colors, false))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
