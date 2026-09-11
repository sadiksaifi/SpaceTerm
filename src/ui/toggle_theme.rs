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
                paint(
                    colors.element_selected,
                    colors.element_selected_foreground,
                    colors.border_selected,
                    colors.text,
                ),
            ),
            values(
                paint(
                    colors.element_hover,
                    colors.element_hover_foreground,
                    colors.border,
                    colors.text,
                ),
                paint(
                    colors.element_selected_hover,
                    colors.element_selected_hover_foreground,
                    colors.border_selected,
                    colors.text,
                ),
            ),
            values(
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.border_focused,
                    colors.text,
                ),
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.border_focused,
                    colors.text,
                ),
            ),
            values(
                paint(
                    colors.element_disabled,
                    colors.icon_disabled,
                    colors.border_disabled,
                    colors.text_disabled,
                ),
                paint(
                    colors.element_disabled,
                    colors.icon_disabled,
                    colors.border_disabled,
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
}
