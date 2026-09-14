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
            gpui_color(colors.ghost_element_selected),
            gpui_color(colors.ghost_element_selected_foreground),
            gpui_color(colors.ghost_element_background),
            gpui_color(colors.ghost_element_hover),
            gpui_color(colors.border_transparent),
            gpui_color(colors.border_focused),
        )
        .trigger_icon_colors(gpui_color(colors.icon), gpui_color(colors.icon_disabled))
        .rows(super::control_theme_catalog::list_rows(colors))
        .hover_background(gpui_color(colors.ghost_element_hover))
        .hover_foreground(gpui_color(colors.ghost_element_hover_foreground)),
        ComboBoxMetrics::new(px(240.0), px(28.0))
            .icon_trigger_size(px(28.0))
            .geometry(px(260.0), px(28.0), px(28.0), px(40.0))
            .spacing(px(4.0), px(8.0), px(18.0), px(6.0))
            .shape(px(6.0), px(1.0))
            .font_sizes(px(12.0), px(11.0))
            .text_geometry(px(16.0), px(12.0)),
    )
    .shadow(super::appearance::control_shadow(colors, false))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_chrome_row_icons_are_independent_from_trigger_icons() {
        let colors = ChromeColors::default();
        let row_icons = ChromeColors {
            row_hover_icon: Color::rgba(0x12345678),
            ..colors.clone()
        };
        let trigger_icons = ChromeColors {
            icon: Color::rgba(0xabcdef98),
            ..colors.clone()
        };
        assert_ne!(theme(&row_icons), theme(&colors));
        assert_ne!(theme(&trigger_icons), theme(&colors));
        assert_ne!(theme(&row_icons), theme(&trigger_icons));
    }
}
