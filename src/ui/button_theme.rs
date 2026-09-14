use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    ButtonMetrics, ButtonPaint, ButtonSizes, ButtonTheme, ButtonVariantStyle, ButtonVariants,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ButtonTheme {
    ButtonTheme::new(
        ButtonVariants::new(
            primary(colors),
            variant(
                paint(
                    colors.element_background,
                    colors.element_foreground,
                    colors.element_icon,
                    colors.element_border,
                ),
                paint(
                    colors.element_hover,
                    colors.element_hover_foreground,
                    colors.element_hover_icon,
                    colors.element_hover_border,
                ),
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.element_active_icon,
                    colors.element_active_border,
                ),
                paint(
                    colors.element_disabled,
                    colors.element_disabled_foreground,
                    colors.element_disabled_icon,
                    colors.element_disabled_border,
                ),
            ),
            outline(colors),
            variant(
                paint(
                    colors.ghost_element_background,
                    colors.ghost_element_foreground,
                    colors.ghost_element_icon,
                    colors.ghost_element_border,
                ),
                paint(
                    colors.ghost_element_hover,
                    colors.ghost_element_hover_foreground,
                    colors.ghost_element_hover_icon,
                    colors.ghost_element_hover_border,
                ),
                paint(
                    colors.ghost_element_active,
                    colors.ghost_element_active_foreground,
                    colors.ghost_element_active_icon,
                    colors.ghost_element_active_border,
                ),
                paint(
                    colors.ghost_element_disabled,
                    colors.ghost_element_disabled_foreground,
                    colors.ghost_element_disabled_icon,
                    colors.ghost_element_disabled_border,
                ),
            ),
            bare(colors),
            destructive(colors),
            variant(
                paint(
                    colors.ghost_element_background,
                    colors.link_text,
                    colors.link_text,
                    colors.ghost_element_border,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.link_text_hover,
                    colors.link_text_hover,
                    colors.ghost_element_border,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.link_text_pressed,
                    colors.link_text_pressed,
                    colors.ghost_element_border,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.link_text_disabled,
                    colors.ghost_element_icon,
                    colors.ghost_element_border,
                ),
            ),
        ),
        ButtonSizes::new(
            ButtonMetrics::new(px(20.0))
                .horizontal_padding(px(4.0))
                .gap(px(4.0))
                .corner_radius(px(4.0))
                .font_size(px(11.0)),
            ButtonMetrics::new(px(24.0))
                .horizontal_padding(px(8.0))
                .gap(px(6.0))
                .corner_radius(px(5.0))
                .font_size(px(12.0)),
            ButtonMetrics::new(px(28.0))
                .horizontal_padding(px(10.0))
                .gap(px(6.0))
                .corner_radius(px(6.0))
                .font_size(px(12.0)),
            ButtonMetrics::new(px(40.0))
                .horizontal_padding(px(12.0))
                .gap(px(8.0))
                .corner_radius(px(6.0))
                .font_size(px(12.0)),
        ),
        gpui_color(colors.border_focused),
    )
}

fn primary(colors: &ChromeColors) -> ButtonVariantStyle {
    variant(
        paint(
            colors.primary_background,
            colors.primary_foreground,
            colors.primary_icon,
            colors.primary_border,
        ),
        paint(
            colors.primary_hover_background,
            colors.primary_hover_foreground,
            colors.primary_hover_icon,
            colors.primary_hover_border,
        ),
        paint(
            colors.primary_pressed_background,
            colors.primary_pressed_foreground,
            colors.primary_pressed_icon,
            colors.primary_pressed_border,
        ),
        paint(
            colors.primary_disabled_background,
            colors.primary_disabled_foreground,
            colors.primary_disabled_icon,
            colors.primary_disabled_border,
        ),
    )
}

fn destructive(colors: &ChromeColors) -> ButtonVariantStyle {
    variant(
        paint(
            colors.destructive_background,
            colors.destructive_foreground,
            colors.destructive_icon,
            colors.destructive_border,
        ),
        paint(
            colors.destructive_hover_background,
            colors.destructive_hover_foreground,
            colors.destructive_hover_icon,
            colors.destructive_hover_border,
        ),
        paint(
            colors.destructive_pressed_background,
            colors.destructive_pressed_foreground,
            colors.destructive_pressed_icon,
            colors.destructive_pressed_border,
        ),
        paint(
            colors.destructive_disabled_background,
            colors.destructive_disabled_foreground,
            colors.destructive_disabled_icon,
            colors.destructive_disabled_border,
        ),
    )
}

fn bare(colors: &ChromeColors) -> ButtonVariantStyle {
    variant(
        paint(
            transparent(),
            colors.text_muted,
            colors.text_muted,
            transparent(),
        ),
        paint(transparent(), colors.text, colors.text, transparent()),
        paint(
            transparent(),
            colors.text_accent,
            colors.text_accent,
            transparent(),
        ),
        paint(
            transparent(),
            colors.text_disabled,
            colors.icon_disabled,
            transparent(),
        ),
    )
}

/// A fully transparent paint value, so a state paints nothing rather than a themed surface.
fn transparent() -> Color {
    Color::rgba(0)
}

fn outline(colors: &ChromeColors) -> ButtonVariantStyle {
    variant(
        paint(
            colors.element_background,
            colors.element_foreground,
            colors.element_icon,
            colors.outline_border,
        ),
        paint(
            colors.element_hover,
            colors.element_hover_foreground,
            colors.element_hover_icon,
            colors.outline_hover_border,
        ),
        paint(
            colors.element_active,
            colors.element_active_foreground,
            colors.element_active_icon,
            colors.outline_pressed_border,
        ),
        paint(
            colors.element_disabled,
            colors.element_disabled_foreground,
            colors.element_disabled_icon,
            colors.outline_disabled_border,
        ),
    )
}

fn variant(
    normal: ButtonPaint,
    hovered: ButtonPaint,
    pressed: ButtonPaint,
    disabled: ButtonPaint,
) -> ButtonVariantStyle {
    ButtonVariantStyle::new(normal, hovered, pressed, disabled)
}

fn paint(background: Color, foreground: Color, icon: Color, border: Color) -> ButtonPaint {
    ButtonPaint::new(
        gpui_color(background),
        gpui_color(foreground),
        gpui_color(border),
    )
    .icon_foreground(gpui_color(icon))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};

    #[test]
    fn primary_and_destructive_consume_complete_independent_state_tuples() {
        use spaceterm_ui::ButtonVariant;
        let mut colors = ChromeColors {
            primary_background: Color::rgba(0x10203040),
            primary_foreground: Color::rgba(0x14233142),
            primary_icon: Color::rgba(0x18263244),
            primary_border: Color::rgba(0x1c293346),
            primary_hover_background: Color::rgba(0x202c3448),
            primary_hover_foreground: Color::rgba(0x242f354a),
            primary_hover_icon: Color::rgba(0x2832364c),
            primary_hover_border: Color::rgba(0x2c35374e),
            primary_pressed_background: Color::rgba(0x30383850),
            primary_pressed_foreground: Color::rgba(0x343b3952),
            primary_pressed_icon: Color::rgba(0x383e3a54),
            primary_pressed_border: Color::rgba(0x3c413b56),
            primary_disabled_background: Color::rgba(0x40443c58),
            primary_disabled_foreground: Color::rgba(0x44473d5a),
            primary_disabled_icon: Color::rgba(0x484a3e5c),
            primary_disabled_border: Color::rgba(0x4c4d3f5e),
            destructive_background: Color::rgba(0x50504060),
            destructive_foreground: Color::rgba(0x54534162),
            destructive_icon: Color::rgba(0x58564264),
            destructive_border: Color::rgba(0x5c594366),
            destructive_hover_background: Color::rgba(0x605c4468),
            destructive_hover_foreground: Color::rgba(0x645f456a),
            destructive_hover_icon: Color::rgba(0x6862466c),
            destructive_hover_border: Color::rgba(0x6c65476e),
            destructive_pressed_background: Color::rgba(0x70684870),
            destructive_pressed_foreground: Color::rgba(0x746b4972),
            destructive_pressed_icon: Color::rgba(0x786e4a74),
            destructive_pressed_border: Color::rgba(0x7c714b76),
            destructive_disabled_background: Color::rgba(0x80744c78),
            destructive_disabled_foreground: Color::rgba(0x84774d7a),
            destructive_disabled_icon: Color::rgba(0x887a4e7c),
            destructive_disabled_border: Color::rgba(0x8c7d4f7e),
            ..ChromeColors::default()
        };
        for (actual, expected) in [
            (
                theme(&colors).paints(ButtonVariant::Primary).normal(),
                paint(
                    colors.primary_background,
                    colors.primary_foreground,
                    colors.primary_icon,
                    colors.primary_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Primary).hovered(),
                paint(
                    colors.primary_hover_background,
                    colors.primary_hover_foreground,
                    colors.primary_hover_icon,
                    colors.primary_hover_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Primary).pressed(),
                paint(
                    colors.primary_pressed_background,
                    colors.primary_pressed_foreground,
                    colors.primary_pressed_icon,
                    colors.primary_pressed_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Primary).disabled(),
                paint(
                    colors.primary_disabled_background,
                    colors.primary_disabled_foreground,
                    colors.primary_disabled_icon,
                    colors.primary_disabled_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Destructive).normal(),
                paint(
                    colors.destructive_background,
                    colors.destructive_foreground,
                    colors.destructive_icon,
                    colors.destructive_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Destructive).hovered(),
                paint(
                    colors.destructive_hover_background,
                    colors.destructive_hover_foreground,
                    colors.destructive_hover_icon,
                    colors.destructive_hover_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Destructive).pressed(),
                paint(
                    colors.destructive_pressed_background,
                    colors.destructive_pressed_foreground,
                    colors.destructive_pressed_icon,
                    colors.destructive_pressed_border,
                ),
            ),
            (
                theme(&colors).paints(ButtonVariant::Destructive).disabled(),
                paint(
                    colors.destructive_disabled_background,
                    colors.destructive_disabled_foreground,
                    colors.destructive_disabled_icon,
                    colors.destructive_disabled_border,
                ),
            ),
        ] {
            assert_eq!(actual, expected);
        }
        let before = theme(&colors).paints(ButtonVariant::Primary);
        colors.selection_background = Color::rgba(0xaabbccdd);
        colors.selection_hover_background = Color::rgba(0x11223344);
        colors.error_background = Color::rgba(0x44556677);
        assert_eq!(theme(&colors).paints(ButtonVariant::Primary), before);
    }

    #[test]
    fn outline_press_should_preserve_the_normal_border() {
        let outline = outline(&ChromeColors::default());

        assert_eq!(outline.normal().border(), outline.pressed().border());
    }

    #[test]
    fn an_emphasized_action_should_keep_a_distinct_hover_in_every_built_in() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let primary = primary(&builtin_chrome_base(appearance));

            assert_ne!(
                primary.normal().background(),
                primary.hovered().background(),
                "{appearance:?} should keep an emphasized action's hover visible"
            );
        }
    }

    #[test]
    fn bare_controls_should_paint_no_surface_in_any_state() {
        let bare = bare(&ChromeColors::default());

        for paint in [
            bare.normal(),
            bare.hovered(),
            bare.pressed(),
            bare.disabled(),
        ] {
            assert_eq!(paint.background().a, 0.0);
            assert_eq!(paint.border().a, 0.0);
        }
    }
}
