use gpui::{Rgba, px, rgba};
use spaceterm_ui::{CommandPaletteMetrics, CommandPalettePaint, CommandPaletteTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> CommandPaletteTheme {
    CommandPaletteTheme::new(
        CommandPalettePaint::new(
            gpui_color(colors.elevated_surface_background),
            gpui_color(colors.border),
            gpui_color(colors.text),
            gpui_color(colors.text_muted),
            gpui_color(colors.text_disabled),
            gpui_color(colors.ghost_element_selected),
            gpui_color(colors.ghost_element_selected_foreground),
            gpui_color(colors.text_accent),
        )
        .icons(gpui_color(colors.icon), gpui_color(colors.icon_disabled))
        .separator(gpui_color(colors.border_variant))
        .hover_background(gpui_color(colors.ghost_element_selected_hover))
        .hover_foreground(gpui_color(colors.ghost_element_selected_hover_foreground))
        .section_foreground(gpui_color(colors.text_muted))
        .footer(gpui_color(colors.text_muted), gpui_color(colors.text_muted)),
        CommandPaletteMetrics::new(px(600.0), px(48.0))
            .single_line_row_height(px(32.0))
            .footer_padding(px(8.0))
            .panel_geometry(px(480.0), px(52.0))
            .viewport_margin(px(16.0))
            .panel_spacing(px(4.0), px(42.0))
            .row_spacing(px(12.0), px(18.0), px(10.0))
            .row_line_gap(px(2.0))
            .section_spacing(px(22.0), px(9.0))
            .footer_height(px(30.0))
            .panel_shape(px(8.0), px(1.0))
            .font_sizes(px(14.0), px(13.0), px(12.0)),
    )
    .shadow(super::appearance::control_shadow(colors, true))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
