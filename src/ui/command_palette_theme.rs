use gpui::{Rgba, px, rgba};
use spaceterm_ui::{CommandPaletteMetrics, CommandPalettePaint, CommandPaletteTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> CommandPaletteTheme {
    CommandPaletteTheme::new(
        CommandPalettePaint::new(
            gpui_color(ACTIVE_THEME.elevated_surface_background),
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.text_muted),
            gpui_color(ACTIVE_THEME.text_disabled),
            gpui_color(ACTIVE_THEME.element_selected),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.text_accent),
        )
        .separator(gpui_color(ACTIVE_THEME.border_variant))
        .hover_background(gpui_color(ACTIVE_THEME.ghost_element_selected))
        .section_foreground(gpui_color(ACTIVE_THEME.text_muted))
        .footer(
            gpui_color(ACTIVE_THEME.text_muted),
            gpui_color(ACTIVE_THEME.text_disabled),
        ),
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
            .font_sizes(px(14.0), px(13.0), px(11.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
