use gpui::{Rgba, rgba};
use spaceterm_ui::ScrollbarTheme;

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ScrollbarTheme {
    ScrollbarTheme::new(
        gpui_color(colors.scrollbar_thumb_background),
        gpui_color(colors.scrollbar_thumb_hover_background),
        gpui_color(colors.scrollbar_thumb_hover_background),
    )
    .borders(
        gpui_color(colors.scrollbar_thumb_border),
        gpui_color(colors.scrollbar_track_border),
    )
    .track_background(gpui_color(colors.scrollbar_track))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
