use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> ComboBoxTheme {
    ComboBoxTheme::new(
        ComboBoxPaint::new(
            gpui_color(ACTIVE_THEME.elevated_surface_background),
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.text_muted),
            gpui_color(ACTIVE_THEME.text_disabled),
            gpui_color(ACTIVE_THEME.element_selected),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.ghost_element_background),
            gpui_color(ACTIVE_THEME.ghost_element_hover),
            gpui_color(ACTIVE_THEME.border_transparent),
            gpui_color(ACTIVE_THEME.border_focused),
        ),
        ComboBoxMetrics::new(px(240.0), px(40.0))
            .icon_trigger_size(px(28.0))
            .geometry(px(260.0), px(28.0), px(30.0), px(46.0))
            .spacing(px(4.0), px(10.0), px(18.0), px(8.0))
            .shape(px(7.0), px(1.0))
            .font_sizes(px(12.0), px(11.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
