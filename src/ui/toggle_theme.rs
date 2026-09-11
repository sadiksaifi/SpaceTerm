use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    ToggleMetrics, TogglePaint, TogglePaints, ToggleSizes, ToggleTheme, ToggleValuePaints,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ToggleTheme {
    let disabled_on = opaque_mix(colors.icon_accent, colors.element_disabled, 0x66);
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
                on_paint(disabled_on, colors.text_disabled),
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
    let background = Color {
        a: 0xff,
        ..background
    };
    paint(
        background,
        contrasting_foreground(background),
        background,
        label,
    )
}

fn contrasting_foreground(background: Color) -> Color {
    let luminance = relative_luminance(background);
    let black_contrast = (luminance + 0.05) / 0.05;
    let white_contrast = 1.05 / (luminance + 0.05);
    if black_contrast >= white_contrast {
        Color::rgb(0x000000)
    } else {
        Color::rgb(0xffffff)
    }
}

fn relative_luminance(color: Color) -> f32 {
    let linear = |channel: u8| {
        let channel = f32::from(channel) / 255.0;
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn opaque_mix(foreground: Color, background: Color, foreground_weight: u8) -> Color {
    let foreground_weight = f32::from(foreground_weight) / 255.0;
    let blend = |foreground: u8, background: u8| {
        f32::from(foreground)
            .mul_add(
                foreground_weight,
                f32::from(background) * (1.0 - foreground_weight),
            )
            .round() as u8
    };
    Color {
        r: blend(foreground.r, background.r),
        g: blend(foreground.g, background.g),
        b: blend(foreground.b, background.b),
        a: 0xff,
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

    #[test]
    fn midtone_accent_should_choose_the_higher_contrast_foreground() {
        let paint = on_paint(Color::rgb(0x6e94b2), Color::rgb(0xffffff));

        assert_eq!(paint.foreground(), rgba(0x000000ff));
    }

    #[test]
    fn translucent_accent_should_resolve_to_an_opaque_selected_surface() {
        let paint = on_paint(Color::rgba(0xffffff66), Color::rgb(0xffffff));

        assert_eq!(paint.background(), rgba(0xffffffff));
    }

    #[test]
    fn disabled_selected_surface_should_mix_accent_without_alpha() {
        let mixed = opaque_mix(Color::rgb(0xffffff), Color::rgb(0x000000), 0x66);

        assert_eq!(mixed, Color::rgb(0x666666));
    }
}
