use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    ToggleMetrics, TogglePaint, TogglePaints, ToggleSizes, ToggleTheme, ToggleValuePaints,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ToggleTheme {
    ToggleTheme::new(
        TogglePaints::new(
            values(
                paint(
                    colors.element_background,
                    colors.icon,
                    colors.border,
                    colors.text,
                ),
                on_paint(colors.icon_accent, colors.text),
            ),
            values(
                paint(
                    colors.element_hover,
                    colors.element_hover_foreground,
                    colors.border,
                    colors.text,
                ),
                on_paint(colors.link_text_hover, colors.text),
            ),
            values(
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.border_focused,
                    colors.text,
                ),
                on_paint(colors.link_text_hover, colors.text),
            ),
            values(
                paint(
                    colors.element_disabled,
                    colors.icon_disabled,
                    colors.border_disabled,
                    colors.text_disabled,
                ),
                on_paint(
                    Color {
                        a: 0x66,
                        ..colors.icon_accent
                    },
                    colors.text_disabled,
                ),
            ),
        ),
        ToggleSizes::new(
            ToggleMetrics::new(px(20.0), px(14.0), px(30.0), px(16.0))
                .label_gap(px(6.0))
                .checkbox_radius(px(3.0))
                .typography(px(11.0), 1.2),
            ToggleMetrics::new(px(24.0), px(16.0), px(34.0), px(18.0))
                .label_gap(px(8.0))
                .checkbox_radius(px(4.0))
                .typography(px(12.0), 1.2),
        ),
        gpui_color(colors.border_focused),
    )
}

fn values(off: TogglePaint, on: TogglePaint) -> ToggleValuePaints {
    ToggleValuePaints::new(off, on)
}

fn paint(background: Color, foreground: Color, border: Color, label: Color) -> TogglePaint {
    TogglePaint::new(
        gpui_color(background),
        gpui_color(foreground),
        gpui_color(border),
        gpui_color(label),
    )
}

fn on_paint(background: Color, label: Color) -> TogglePaint {
    paint(
        background,
        contrasting_foreground(background),
        background,
        label,
    )
}

fn contrasting_foreground(background: Color) -> Color {
    let linear = |channel: u8| {
        let channel = f32::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(background.r)
        + 0.7152 * linear(background.g)
        + 0.0722 * linear(background.b);
    if luminance > 0.5 {
        Color::rgb(0x000000)
    } else {
        Color::rgb(0xffffff)
    }
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_should_scale_toggle_geometry() {
        let theme = theme(&ChromeColors::default());

        assert_ne!(theme, theme.scaled_metrics(1.25, 1.25));
    }

    #[test]
    fn on_paint_should_use_accent_as_the_complete_surface() {
        let colors = ChromeColors::default();
        let paint = on_paint(colors.icon_accent, colors.text);

        assert_eq!(
            (paint.background(), paint.border()),
            (
                gpui_color(colors.icon_accent),
                gpui_color(colors.icon_accent)
            )
        );
    }
}
