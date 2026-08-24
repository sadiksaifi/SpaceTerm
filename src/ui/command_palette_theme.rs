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
            gpui_color(ACTIVE_THEME.editor_background),
            gpui_color(ACTIVE_THEME.border_focused),
            gpui_color(ACTIVE_THEME.players[0].selection),
            gpui_color(ACTIVE_THEME.players[0].cursor),
        ),
        CommandPaletteMetrics::new(px(560.0), px(44.0))
            .panel_geometry(px(420.0), px(64.0))
            .viewport_margin(px(16.0))
            .panel_spacing(px(6.0), px(38.0))
            .row_spacing(px(10.0), px(18.0), px(8.0))
            .panel_shape(px(10.0), px(1.0))
            .font_sizes(px(13.0), px(11.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
