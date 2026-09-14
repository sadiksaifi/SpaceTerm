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
                    colors.toggle_off_background,
                    colors.toggle_off_mark,
                    colors.toggle_off_border,
                    colors.toggle_off_label,
                ),
                paint(
                    colors.toggle_on_background,
                    colors.toggle_on_mark,
                    colors.toggle_on_border,
                    colors.toggle_on_label,
                ),
            ),
            values(
                paint(
                    colors.toggle_off_hover_background,
                    colors.toggle_off_hover_mark,
                    colors.toggle_off_hover_border,
                    colors.toggle_off_hover_label,
                ),
                paint(
                    colors.toggle_on_hover_background,
                    colors.toggle_on_hover_mark,
                    colors.toggle_on_hover_border,
                    colors.toggle_on_hover_label,
                ),
            ),
            values(
                paint(
                    colors.toggle_off_pressed_background,
                    colors.toggle_off_pressed_mark,
                    colors.toggle_off_pressed_border,
                    colors.toggle_off_pressed_label,
                ),
                paint(
                    colors.toggle_on_pressed_background,
                    colors.toggle_on_pressed_mark,
                    colors.toggle_on_pressed_border,
                    colors.toggle_on_pressed_label,
                ),
            ),
            values(
                paint(
                    colors.toggle_off_disabled_background,
                    colors.toggle_off_disabled_mark,
                    colors.toggle_off_disabled_border,
                    colors.toggle_off_disabled_label,
                ),
                paint(
                    colors.toggle_on_disabled_background,
                    colors.toggle_on_disabled_mark,
                    colors.toggle_on_disabled_border,
                    colors.toggle_on_disabled_label,
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
    fn every_toggle_value_and_interaction_uses_its_exact_paint() {
        let colors = ChromeColors {
            toggle_off_background: Color::rgba(0x10203040),
            toggle_off_mark: Color::rgba(0x14233142),
            toggle_off_border: Color::rgba(0x18263244),
            toggle_off_label: Color::rgba(0x1c293346),
            toggle_off_hover_background: Color::rgba(0x202c3448),
            toggle_off_hover_mark: Color::rgba(0x242f354a),
            toggle_off_hover_border: Color::rgba(0x2832364c),
            toggle_off_hover_label: Color::rgba(0x2c35374e),
            toggle_off_pressed_background: Color::rgba(0x30383850),
            toggle_off_pressed_mark: Color::rgba(0x343b3952),
            toggle_off_pressed_border: Color::rgba(0x383e3a54),
            toggle_off_pressed_label: Color::rgba(0x3c413b56),
            toggle_off_disabled_background: Color::rgba(0x40443c58),
            toggle_off_disabled_mark: Color::rgba(0x44473d5a),
            toggle_off_disabled_border: Color::rgba(0x484a3e5c),
            toggle_off_disabled_label: Color::rgba(0x4c4d3f5e),
            toggle_on_background: Color::rgba(0x50504060),
            toggle_on_mark: Color::rgba(0x54534162),
            toggle_on_border: Color::rgba(0x58564264),
            toggle_on_label: Color::rgba(0x5c594366),
            toggle_on_hover_background: Color::rgba(0x605c4468),
            toggle_on_hover_mark: Color::rgba(0x645f456a),
            toggle_on_hover_border: Color::rgba(0x6862466c),
            toggle_on_hover_label: Color::rgba(0x6c65476e),
            toggle_on_pressed_background: Color::rgba(0x70684870),
            toggle_on_pressed_mark: Color::rgba(0x746b4972),
            toggle_on_pressed_border: Color::rgba(0x786e4a74),
            toggle_on_pressed_label: Color::rgba(0x7c714b76),
            toggle_on_disabled_background: Color::rgba(0x80744c78),
            toggle_on_disabled_mark: Color::rgba(0x84774d7a),
            toggle_on_disabled_border: Color::rgba(0x887a4e7c),
            toggle_on_disabled_label: Color::rgba(0x8c7d4f7e),
            ..ChromeColors::default()
        };
        let theme = theme(&colors);
        assert_eq!(
            theme.paint(false, true, false, false),
            paint(
                colors.toggle_off_background,
                colors.toggle_off_mark,
                colors.toggle_off_border,
                colors.toggle_off_label
            )
        );
        assert_eq!(
            theme.paint(false, true, true, false),
            paint(
                colors.toggle_off_hover_background,
                colors.toggle_off_hover_mark,
                colors.toggle_off_hover_border,
                colors.toggle_off_hover_label
            )
        );
        assert_eq!(
            theme.paint(false, true, true, true),
            paint(
                colors.toggle_off_pressed_background,
                colors.toggle_off_pressed_mark,
                colors.toggle_off_pressed_border,
                colors.toggle_off_pressed_label
            )
        );
        assert_eq!(
            theme.paint(false, false, true, true),
            paint(
                colors.toggle_off_disabled_background,
                colors.toggle_off_disabled_mark,
                colors.toggle_off_disabled_border,
                colors.toggle_off_disabled_label
            )
        );
        assert_eq!(
            theme.paint(true, true, false, false),
            paint(
                colors.toggle_on_background,
                colors.toggle_on_mark,
                colors.toggle_on_border,
                colors.toggle_on_label
            )
        );
        assert_eq!(
            theme.paint(true, true, true, false),
            paint(
                colors.toggle_on_hover_background,
                colors.toggle_on_hover_mark,
                colors.toggle_on_hover_border,
                colors.toggle_on_hover_label
            )
        );
        assert_eq!(
            theme.paint(true, true, true, true),
            paint(
                colors.toggle_on_pressed_background,
                colors.toggle_on_pressed_mark,
                colors.toggle_on_pressed_border,
                colors.toggle_on_pressed_label
            )
        );
        assert_eq!(
            theme.paint(true, false, true, true),
            paint(
                colors.toggle_on_disabled_background,
                colors.toggle_on_disabled_mark,
                colors.toggle_on_disabled_border,
                colors.toggle_on_disabled_label
            )
        );
    }

    #[test]
    fn theme_should_scale_toggle_geometry() {
        let theme = theme(&ChromeColors::default());

        assert_ne!(theme, theme.scaled_metrics(1.25, 1.25));
    }

    #[test]
    fn authored_on_state_alpha_and_mark_are_preserved() {
        let colors = ChromeColors {
            toggle_on_background: Color::rgba(0x12345678),
            toggle_on_mark: Color::rgba(0xabcdef98),
            ..ChromeColors::default()
        };
        let paint = theme(&colors).paint(true, true, false, false);
        assert_eq!(paint.background(), gpui_color(colors.toggle_on_background));
        assert_eq!(paint.foreground(), gpui_color(colors.toggle_on_mark));
    }
}
