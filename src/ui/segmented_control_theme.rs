use crate::ui::appearance::gpui_color;
use gpui::px;
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

    #[gpui::test]
    fn theme_should_scale_segmented_geometry(cx: &mut gpui::TestAppContext) {
        use gpui::prelude::*;
        struct Fixture;
        impl gpui::Render for Fixture {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div().child(
                    spaceterm_ui::SegmentedControl::new(
                        "geometry-segment",
                        "Mode",
                        &false,
                        vec![
                            spaceterm_ui::SegmentedOption::new(false, "Off")
                                .debug_selector("geometry-option"),
                        ],
                    )
                    .unwrap()
                    .on_change(|_, _, _| {}),
                )
            }
        }
        let base = theme(&ChromeColors::default());
        let scaled = base.scaled_metrics(1.25, 1.25);
        for selected in [false, true] {
            for enabled in [false, true] {
                for hovered in [false, true] {
                    for pressed in [false, true] {
                        assert_eq!(
                            base.paint(selected, enabled, hovered, pressed),
                            scaled.paint(selected, enabled, hovered, pressed)
                        );
                    }
                }
            }
        }
        cx.set_global(scaled);
        let (_, cx) = cx.add_window_view(|_, _| Fixture);
        cx.run_until_parked();
        let option = cx.debug_bounds("geometry-option").expect("option paints");
        assert_eq!(option.size.height, px(30.0));
        assert_eq!(option.size.width, px(80.0));
        let body = ChromeTypography::default().style(TextRole::Body).clone();
        assert_eq!(
            cx.debug_bounds("geometry-option-label")
                .unwrap()
                .size
                .height,
            body.line_height * 1.25
        );
        let expected_label_width = cx.update(|window, _| {
            window
                .text_system()
                .shape_line(
                    "Off".into(),
                    body.size * 1.25,
                    &[gpui::TextRun {
                        len: 3,
                        font: spaceterm_ui::ControlTypography::default().regular().clone(),
                        color: gpui::black(),
                        background_color: None,
                        underline: None,
                        strikethrough: None,
                    }],
                    None,
                )
                .width
        });
        assert_eq!(
            cx.debug_bounds("geometry-option-label").unwrap().size.width,
            expected_label_width
        );
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

        let normal = theme(&base);
        for (changed, state, background, label, unchanged) in [
            (
                selected.clone(),
                (true, false),
                Some(selected.selection_background),
                None,
                (false, false),
            ),
            (
                unselected_label.clone(),
                (false, false),
                None,
                Some(unselected_label.text_secondary),
                (true, false),
            ),
            (
                hovered.clone(),
                (false, true),
                Some(hovered.ghost_element_hover),
                None,
                (true, true),
            ),
            (
                selected_hovered.clone(),
                (true, true),
                Some(selected_hovered.selection_hover_background),
                None,
                (false, true),
            ),
        ] {
            let changed_theme = theme(&changed);
            let paint = changed_theme.paint(state.0, true, state.1, false);
            if let Some(background) = background {
                assert_eq!(paint.background(), gpui_color(background));
            }
            if let Some(label) = label {
                assert_eq!(paint.label(), gpui_color(label));
            }
            assert_eq!(
                changed_theme.paint(unchanged.0, true, unchanged.1, false),
                normal.paint(unchanged.0, true, unchanged.1, false)
            );
        }
    }

    #[gpui::test]
    fn show_borders_should_outline_each_unselected_state(cx: &mut gpui::TestAppContext) {
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

        use gpui::prelude::*;
        struct Fixture;
        impl gpui::Render for Fixture {
            fn render(
                &mut self,
                _: &mut gpui::Window,
                _: &mut gpui::Context<Self>,
            ) -> impl gpui::IntoElement {
                gpui::div().child(
                    spaceterm_ui::SegmentedControl::new(
                        "border-track",
                        "Mode",
                        &false,
                        vec![spaceterm_ui::SegmentedOption::new(false, "Off")],
                    )
                    .unwrap()
                    .debug_selector("border-track")
                    .on_change(|_, _, _| {}),
                )
            }
        }
        cx.set_global(theme);
        let (_, cx) = cx.add_window_view(|_, _| Fixture);
        cx.run_until_parked();
        let track = cx.debug_bounds("border-track").expect("track paints");
        cx.update(|window, _| {
            let quads = window.painted_quads();
            let track_quad = quads
                .iter()
                .find(|quad| {
                    quad.bounds == track.scale(window.scale_factor())
                        && quad.border_widths.left.0 > 0.0
                })
                .expect("track quad paints");
            assert_eq!(
                track_quad.border_color,
                gpui_color(colors.element_border).into()
            );
        });
    }
}
