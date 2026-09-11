use gpui::{Rgba, px, rgba};
use spaceterm_ui::{
    SegmentedControlTheme, SegmentedMetrics, SegmentedPaint, SegmentedPaints, SegmentedSizes,
    SegmentedValuePaints,
};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> SegmentedControlTheme {
    SegmentedControlTheme::new(
        SegmentedPaints::new(
            values(
                paint(
                    colors.border_transparent,
                    colors.text_secondary,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_selected,
                    colors.element_selected_foreground,
                    colors.border_selected,
                ),
            ),
            values(
                paint(
                    colors.ghost_element_hover,
                    colors.ghost_element_hover_foreground,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_selected_hover,
                    colors.element_selected_hover_foreground,
                    colors.border_selected,
                ),
            ),
            values(
                paint(
                    colors.ghost_element_active,
                    colors.ghost_element_active_foreground,
                    colors.border_variant,
                ),
                paint(
                    colors.element_active,
                    colors.element_active_foreground,
                    colors.border_focused,
                ),
            ),
            values(
                paint(
                    colors.border_transparent,
                    colors.text_disabled,
                    colors.border_transparent,
                ),
                paint(
                    colors.element_disabled,
                    colors.element_disabled_foreground,
                    colors.border_disabled,
                ),
            ),
        ),
        SegmentedSizes::new(
            SegmentedMetrics::new(px(22.0), px(0.0), px(56.0), px(0.0))
                .horizontal_padding(px(10.0))
                .radius(px(4.0))
                .typography(px(12.0), 1.2),
            SegmentedMetrics::new(px(26.0), px(52.0), px(88.0), px(10.0))
                .horizontal_padding(px(12.0))
                .radius(px(7.0))
                .preview_gap(px(7.0))
                .typography(px(12.0), 1.2),
        ),
        gpui_color(colors.element_background),
        gpui_color(colors.border),
        gpui_color(colors.border_focused),
    )
    .selected_shadow(super::appearance::control_shadow(colors, false))
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
    fn theme_should_scale_segmented_geometry() {
        let theme = theme(&ChromeColors::default());

        assert_ne!(theme, theme.scaled_metrics(1.25, 1.25));
    }

    #[test]
    fn selected_and_unselected_options_should_consume_distinct_roles() {
        let base = ChromeColors::default();
        let selected = ChromeColors {
            element_selected: Color::rgb(0x123456),
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

        for changed in [selected, unselected_label, hovered] {
            assert_ne!(theme(&base), theme(&changed));
        }
    }
}
