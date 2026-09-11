use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    ButtonMetrics, ButtonPaint, ButtonSizes, ButtonTheme, ButtonVariantStyle, ButtonVariants,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ButtonTheme {
    ButtonTheme::new(
        ButtonVariants::new(
            variant(
                paint(
                    colors.element_selected,
                    colors.element_selected_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_selected_hover,
                    colors.element_selected_hover_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_disabled,
                    colors.element_disabled_foreground,
                    colors.icon_disabled,
                    colors.border_transparent,
                ),
            ),
            variant(
                paint(
                    colors.element_background,
                    colors.element_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_hover,
                    colors.element_hover_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_disabled,
                    colors.element_disabled_foreground,
                    colors.icon_disabled,
                    colors.border_transparent,
                ),
            ),
            outline(colors),
            variant(
                paint(
                    colors.ghost_element_background,
                    colors.ghost_element_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_hover,
                    colors.ghost_element_hover_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_active,
                    colors.ghost_element_active_foreground,
                    colors.icon,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_disabled,
                    colors.ghost_element_disabled_foreground,
                    colors.icon_disabled,
                    colors.border_transparent,
                ),
            ),
            bare(colors),
            variant(
                paint(
                    colors.error_background,
                    colors.error,
                    colors.error,
                    colors.error_border,
                ),
                paint(
                    colors.element_hover,
                    colors.error,
                    colors.error,
                    colors.error_border,
                ),
                paint(
                    colors.element_active,
                    colors.error,
                    colors.error,
                    colors.error_border,
                ),
                paint(
                    colors.element_disabled,
                    colors.element_disabled_foreground,
                    colors.icon_disabled,
                    colors.border_disabled,
                ),
            ),
            variant(
                paint(
                    colors.ghost_element_background,
                    colors.link_text,
                    colors.link_text,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.link_text_hover,
                    colors.link_text_hover,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.text_accent,
                    colors.text_accent,
                    colors.border_transparent,
                ),
                paint(
                    colors.ghost_element_background,
                    colors.ghost_element_disabled_foreground,
                    colors.icon_disabled,
                    colors.border_transparent,
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

/// A control with no surface in any state, which brightens rather than fills on interaction.
///
/// Chrome that must read as floating uses this: nothing paints behind the glyph, so the control
/// carries the same visual weight as the text beside it.
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
            colors.icon,
            colors.border,
        ),
        paint(
            colors.element_hover,
            colors.element_hover_foreground,
            colors.icon,
            colors.border,
        ),
        paint(
            colors.element_active,
            colors.element_active_foreground,
            colors.icon,
            colors.border,
        ),
        paint(
            colors.element_disabled,
            colors.element_disabled_foreground,
            colors.icon_disabled,
            colors.border_disabled,
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

    #[test]
    fn outline_press_should_preserve_the_normal_border() {
        let outline = outline(&ChromeColors::default());

        assert_eq!(outline.normal().border(), outline.pressed().border());
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
