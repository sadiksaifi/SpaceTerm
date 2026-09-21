use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    SegmentedControlTheme, SegmentedMetrics, SegmentedPaint, SegmentedPaints, SegmentedSizes,
    SegmentedValuePaints,
};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

#[cfg(test)]
pub(super) fn theme(colors: &ChromeColors) -> SegmentedControlTheme {
    prepared(colors, &ChromeTypography::default(), false)
}

pub(super) fn prepared(
    colors: &ChromeColors,
    typography: &ChromeTypography,
    show_borders: bool,
) -> SegmentedControlTheme {
    let border = |ordinary, accessible| {
        if show_borders { accessible } else { ordinary }
    };
    let body_size = typography.style(TextRole::Body).size;
    let body_line_height =
        f32::from(typography.style(TextRole::Body).line_height) / f32::from(body_size);
    SegmentedControlTheme::new(
        SegmentedPaints::new(
            values(
                paint(
                    colors.border_transparent,
                    colors.text_secondary,
                    border(colors.border_transparent, colors.ghost_element_border),
                ),
                paint(
                    colors.selection_background,
                    colors.selection_foreground,
                    colors.selection_border,
                ),
            ),
            values(
                paint(
                    colors.ghost_element_hover,
                    colors.ghost_element_hover_foreground,
                    border(colors.border_transparent, colors.ghost_element_hover_border),
                ),
                paint(
                    colors.selection_hover_background,
                    colors.selection_hover_foreground,
                    colors.selection_hover_border,
                ),
            ),
            values(
                paint(
                    colors.ghost_element_active,
                    colors.ghost_element_active_foreground,
                    border(colors.border_variant, colors.ghost_element_active_border),
                ),
                paint(
                    colors.selection_pressed_background,
                    colors.selection_pressed_foreground,
                    colors.selection_pressed_border,
                ),
            ),
            values(
                paint(
                    colors.border_transparent,
                    colors.text_disabled,
                    border(
                        colors.border_transparent,
                        colors.ghost_element_disabled_border,
                    ),
                ),
                paint(
                    colors.selection_disabled_background,
                    colors.selection_disabled_foreground,
                    colors.selection_disabled_border,
                ),
            ),
        ),
        SegmentedSizes::new(
            // The option height plus the track border and padding matches the shared 28 px control
            // height, so a segmented control lines up with buttons, steppers, and selectors.
            SegmentedMetrics::new(px(24.0), px(0.0), px(64.0), px(0.0))
                .horizontal_padding(px(10.0))
                .radius(RadiusRole::Control.pixels())
                .typography(body_size, body_line_height),
            // A card is a thumbnail with its name under it, sized so a row of them reads beside a
            // label rather than towering over it.
            SegmentedMetrics::new(px(20.0), px(38.0), px(72.0), px(8.0))
                .horizontal_padding(px(10.0))
                .vertical_padding(px(7.0))
                .radius(RadiusRole::Card.pixels())
                .preview_gap(px(6.0))
                .typography(body_size, body_line_height),
        ),
        gpui_color(colors.element_background),
        gpui_color(border(colors.border, colors.element_border)),
        gpui_color(colors.focus_ring),
    )
    // A resting segment uses its fill and hairline for selection. A drop shadow remains visible
    // through translucent fills and makes the selected segment look recessed.
    .selected_shadow(spaceterm_ui::ControlShadow::none())
}

fn values(unselected: SegmentedPaint, selected: SegmentedPaint) -> SegmentedValuePaints {
    SegmentedValuePaints::new(unselected, selected)
}

fn paint(background: Color, label: Color, border: Color) -> SegmentedPaint {
    SegmentedPaint::new(
        gpui_color(background),
        gpui_color(label),
        gpui_color(border),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_pressed_and_disabled_remain_selection_paints() {
        let colors = ChromeColors {
            selection_pressed_background: Color::rgba(0x11223344),
            selection_pressed_foreground: Color::rgba(0x55667788),
            selection_pressed_border: Color::rgba(0x99aabbcc),
            selection_disabled_background: Color::rgba(0x12345678),
            selection_disabled_foreground: Color::rgba(0x23456789),
            selection_disabled_border: Color::rgba(0x3456789a),
            ..ChromeColors::default()
        };
        assert_eq!(
            theme(&colors).paint(true, true, true, true),
            paint(
                colors.selection_pressed_background,
                colors.selection_pressed_foreground,
                colors.selection_pressed_border
            )
        );
        assert_eq!(
            theme(&colors).paint(true, false, true, true),
            paint(
                colors.selection_disabled_background,
                colors.selection_disabled_foreground,
                colors.selection_disabled_border
            )
        );
    }

    #[test]
    fn theme_should_scale_segmented_geometry() {
        let theme = theme(&ChromeColors::default());

        assert_ne!(theme, theme.scaled_metrics(1.25, 1.25));
    }

    #[test]
    fn selected_and_unselected_options_should_consume_distinct_roles() {
        let base = ChromeColors::default();
        let selected = ChromeColors {
            selection_background: Color::rgb(0x123456),
            ..base.clone()
        };
        let unselected_label = ChromeColors {
            text_secondary: Color::rgb(0xabcdef),
            ..base.clone()
        };
        let hovered = ChromeColors {
            ghost_element_hover: Color::rgb(0x654321),
            ..base.clone()
        };
        // A selected option consumes its own hovered role, so a scheme can distinguish the two.
        let selected_hovered = ChromeColors {
            selection_hover_background: Color::rgb(0x654322),
            ..base.clone()
        };

        for changed in [selected, unselected_label, hovered, selected_hovered] {
            assert_ne!(theme(&base), theme(&changed));
        }
    }

    #[test]
    fn show_borders_should_outline_each_unselected_state() {
        let colors = ChromeColors {
            ghost_element_border: Color::rgb(0x112233),
            ghost_element_hover_border: Color::rgb(0x223344),
            ghost_element_active_border: Color::rgb(0x334455),
            ghost_element_disabled_border: Color::rgb(0x445566),
            element_border: Color::rgb(0x556677),
            ..ChromeColors::default()
        };
        let theme = prepared(&colors, &ChromeTypography::default(), true);

        assert_eq!(
            theme.paint(false, true, false, false).border(),
            gpui_color(colors.ghost_element_border)
        );
        assert_eq!(
            theme.paint(false, true, true, false).border(),
            gpui_color(colors.ghost_element_hover_border)
        );
        assert_eq!(
            theme.paint(false, true, false, true).border(),
            gpui_color(colors.ghost_element_active_border)
        );
        assert_eq!(
            theme.paint(false, false, true, true).border(),
            gpui_color(colors.ghost_element_disabled_border),
            "disabled precedence should retain its Show Borders edge"
        );
        assert_ne!(
            theme,
            prepared(&colors, &ChromeTypography::default(), false),
            "Show Borders must also replace the segmented track edge"
        );
    }
}
