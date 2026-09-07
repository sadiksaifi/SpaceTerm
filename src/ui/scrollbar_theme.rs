use gpui::{Rgba, rgba};
use spaceterm_ui::ScrollbarTheme;

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> ScrollbarTheme {
    ScrollbarTheme::new(
        gpui_color(ACTIVE_THEME.scrollbar_thumb_background),
        gpui_color(ACTIVE_THEME.scrollbar_thumb_hover_background),
        gpui_color(ACTIVE_THEME.scrollbar_thumb_hover_background),
    )
    .borders(
        gpui_color(ACTIVE_THEME.scrollbar_thumb_border),
        gpui_color(ACTIVE_THEME.scrollbar_track_border),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
