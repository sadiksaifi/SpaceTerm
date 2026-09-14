use gpui::{Rgba, rgba};
use spaceterm_ui::ScrollbarTheme;

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ScrollbarTheme {
    ScrollbarTheme::new(
        gpui_color(colors.scrollbar_thumb_background),
        gpui_color(colors.scrollbar_thumb_hover_background),
        gpui_color(colors.scrollbar_thumb_active_background),
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dragging_uses_its_authored_slot_even_while_hovered() {
        let colors = ChromeColors {
            scrollbar_thumb_background: Color::rgba(0x11223344),
            scrollbar_thumb_hover_background: Color::rgba(0x55667788),
            scrollbar_thumb_active_background: Color::rgba(0x99aabbcc),
            ..ChromeColors::default()
        };
        let theme = theme(&colors);
        assert_eq!(
            theme.thumb_color(false, false),
            gpui_color(colors.scrollbar_thumb_background)
        );
        assert_eq!(
            theme.thumb_color(true, false),
            gpui_color(colors.scrollbar_thumb_hover_background)
        );
        for hovered in [false, true] {
            assert_eq!(
                theme.thumb_color(hovered, true),
                gpui_color(colors.scrollbar_thumb_active_background)
            );
        }
    }
}
