use crate::ui::appearance::gpui_color;
use gpui::px;
use spaceterm_ui::{
    SegmentedControlTheme, SegmentedMetrics, SegmentedPaint, SegmentedPaints, SegmentedSizes,
    SegmentedValuePaints,
};

use crate::appearance::{Appearance, ChromeColors, Color};
use crate::ui::appearance::ChromeAppearance;
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

/// The share of the accent a disabled selection keeps, from the approved design.
const DISABLED_ACCENT_OPACITY: u8 = 102;
/// The share of the selected label a disabled selection keeps, from the approved design.
const DISABLED_LABEL_OPACITY: u8 = 140;
/// The share of label ink in the hairline between two unselected options.
const DIVIDER_OPACITY: u8 = 56;

/// What the window and accessibility settings ask of a segmented control.
#[derive(Clone, Copy, Debug)]
pub(in crate::ui) struct SegmentedPolicy {
    /// Show Borders outlines every option.
    show_borders: bool,
    /// Light mode, Increase Contrast, and Show Borders keep the edge of the track and of a neutral
    /// selection. In dark mode both read by their fill alone.
    edges: bool,
    /// A window that is not key draws selection in the neutral selection roles, as AppKit does.
    active: bool,
}

impl SegmentedPolicy {
    pub(in crate::ui) fn prepared(appearance: &ChromeAppearance) -> Self {
        let show_borders = appearance.capabilities.show_borders;
        Self {
            show_borders,
            edges: appearance.appearance == Appearance::Light
                || appearance.capabilities.increase_contrast
                || show_borders,
            active: appearance.active,
        }
    }
}

#[cfg(test)]
pub(super) fn theme(colors: &ChromeColors) -> SegmentedControlTheme {
    prepared(
        colors,
        &ChromeTypography::default(),
        SegmentedPolicy {
            show_borders: false,
            edges: false,
            active: true,
        },
    )
}

/// In a key window, selection takes the accent of the primary button, so a selected option and
/// the Create button agree. List rows and the Tab strip keep their neutral selection.
pub(in crate::ui) fn prepared(
    colors: &ChromeColors,
    typography: &ChromeTypography,
    policy: SegmentedPolicy,
) -> SegmentedControlTheme {
    let border = |ordinary, accessible| {
        if policy.show_borders {
            accessible
        } else {
            ordinary
        }
    };
    let unselected = [
        paint(
            colors.border_transparent,
            colors.text_secondary,
            border(colors.border_transparent, colors.ghost_element_border),
        ),
        paint(
            colors.ghost_element_hover,
            colors.ghost_element_hover_foreground,
            border(colors.border_transparent, colors.ghost_element_hover_border),
        ),
        paint(
            colors.ghost_element_active,
            colors.ghost_element_active_foreground,
            border(colors.border_variant, colors.ghost_element_active_border),
        ),
        paint(
            colors.border_transparent,
            colors.text_disabled,
            border(
                colors.border_transparent,
                colors.ghost_element_disabled_border,
            ),
        ),
    ];
    // The accent fill states the selected option's shape, so it has no rim. Its edge is clear
    // rather than the primary button's border, which reduced Opacity leaves opaque over a
    // translucent fill. A neutral selection may sit close to its track, so it keeps an edge
    // wherever the track keeps one.
    let rimless = colors.border_transparent;
    // A disabled selection is a dimmed accent chip with a dimmed label. The primary button's
    // disabled roles become the ordinary disabled button fill, which a segment cannot tell apart
    // from its unselected neighbours.
    let dimmed_accent = colors
        .primary_background
        .multiply_opacity(DISABLED_ACCENT_OPACITY);
    let dimmed_label = colors
        .primary_foreground
        .multiply_opacity(DISABLED_LABEL_OPACITY);
    let neutral = |fill: Color, label: Color, edge: Color| {
        paint(fill, label, if policy.edges { edge } else { rimless })
    };
    let selected = if policy.active {
        [
            paint(
                colors.primary_background,
                colors.primary_foreground,
                rimless,
            ),
            paint(
                colors.primary_hover_background,
                colors.primary_hover_foreground,
                rimless,
            ),
            paint(
                colors.primary_pressed_background,
                colors.primary_pressed_foreground,
                rimless,
            ),
            paint(dimmed_accent, dimmed_label, rimless),
        ]
    } else {
        [
            neutral(
                colors.selection_background,
                colors.selection_foreground,
                colors.selection_border,
            ),
            neutral(
                colors.selection_hover_background,
                colors.selection_hover_foreground,
                colors.selection_hover_border,
            ),
            neutral(
                colors.selection_pressed_background,
                colors.selection_pressed_foreground,
                colors.selection_pressed_border,
            ),
            neutral(
                colors.selection_disabled_background,
                colors.selection_disabled_foreground,
                colors.selection_disabled_border,
            ),
        ]
    };
    // A card keeps a neutral fill so its preview reads in its own colors, and its border is the
    // selection ring: the accent in a key window and a neutral rung otherwise.
    let ring = if policy.active {
        [
            colors.primary_background,
            colors.primary_hover_background,
            colors.primary_pressed_background,
            dimmed_accent,
        ]
    } else {
        [
            colors.border_selected,
            colors.border_selected,
            colors.border_selected,
            colors.border_disabled,
        ]
    };
    let card_selected = [
        (colors.selection_background, colors.selection_foreground),
        (
            colors.selection_hover_background,
            colors.selection_hover_foreground,
        ),
        (
            colors.selection_pressed_background,
            colors.selection_pressed_foreground,
        ),
        (
            colors.selection_disabled_background,
            colors.selection_disabled_foreground,
        ),
    ];
    let description = [
        colors.text_secondary,
        colors.text_secondary,
        colors.text_secondary,
        colors.text_disabled,
    ];
    let regular = std::array::from_fn(|state| values(unselected[state], selected[state]));
    let card = std::array::from_fn(|state| {
        let (fill, label) = card_selected[state];
        let description = gpui_color(description[state]);
        values(
            unselected[state].description(description),
            paint(fill, label, ring[state]).description(description),
        )
    });
    let body_size = typography.style(TextRole::Body).size;
    let body_line_height =
        f32::from(typography.style(TextRole::Body).line_height) / f32::from(body_size);
    SegmentedControlTheme::new(
        catalog(regular),
        SegmentedSizes::new(
            // The option height plus the track border and padding matches the shared 28 px control
            // height, so a segmented control lines up with buttons, steppers, and selectors.
            SegmentedMetrics::new(px(24.0), px(0.0), px(64.0), px(0.0))
                .horizontal_padding(px(10.0))
                .radius(RadiusRole::Control.pixels())
                .typography(body_size, body_line_height),
            // A card is a thumbnail with its name under it, sized so a row of them reads beside a
            // label rather than towering over it. Its border is the selection ring.
            SegmentedMetrics::new(px(20.0), px(38.0), px(72.0), px(8.0))
                .horizontal_padding(px(10.0))
                .vertical_padding(px(7.0))
                .radius(RadiusRole::Card.pixels())
                .border_width(px(2.0))
                .preview_gap(px(6.0))
                .typography(body_size, body_line_height),
        ),
        gpui_color(colors.element_background),
        gpui_color(if policy.edges {
            border(colors.border, colors.element_border)
        } else {
            colors.border_transparent
        }),
        gpui_color(colors.focus_ring),
    )
    .card_paints(catalog(card))
    // The divider is label ink at low opacity, so it keeps the same step over any track. The
    // separator role is tuned for opaque surfaces and fades into a translucent dark track.
    .divider(gpui_color(colors.text.multiply_opacity(DIVIDER_OPACITY)))
    // A resting segment uses its fill for selection. A drop shadow remains visible through
    // translucent fills and makes the selected segment look recessed.
    .selected_shadow(spaceterm_ui::ControlShadow::none())
}

/// Orders normal, hovered, pressed, and disabled paint into the control's catalog.
fn catalog(states: [SegmentedValuePaints; 4]) -> SegmentedPaints {
    let [normal, hovered, pressed, disabled] = states;
    SegmentedPaints::new(normal, hovered, pressed, disabled)
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

    fn policy(active: bool, show_borders: bool, edges: bool) -> SegmentedPolicy {
        SegmentedPolicy {
            show_borders,
            edges,
            active,
        }
    }

    fn selection_colors() -> ChromeColors {
        ChromeColors {
            primary_background: Color::rgb(0x1f66d1),
            primary_foreground: Color::rgb(0xffffff),
            primary_border: Color::rgb(0x0a3d8f),
            primary_pressed_background: Color::rgb(0x1a56b0),
            primary_pressed_foreground: Color::rgb(0xfefefe),
            primary_pressed_border: Color::rgb(0x0a3d8f),
            // Final disabled preparation gives the primary button the ordinary disabled fill.
            primary_disabled_background: Color::rgba(0x00000000),
            primary_disabled_foreground: Color::rgb(0x8a8a8a),
            primary_disabled_border: Color::rgb(0x0a3d8f),
            selection_background: Color::rgb(0x4a4a4a),
            selection_foreground: Color::rgb(0xe4e4e4),
            selection_border: Color::rgb(0x8e8e8e),
            selection_disabled_background: Color::rgb(0x333333),
            selection_disabled_foreground: Color::rgb(0x606060),
            selection_disabled_border: Color::rgb(0x555555),
            border_selected: Color::rgb(0x777777),
            ..ChromeColors::default()
        }
    }

    #[test]
    fn a_key_window_selects_with_the_primary_accent_and_no_rim() {
        let colors = selection_colors();
        let theme = prepared(
            &colors,
            &ChromeTypography::default(),
            policy(true, false, false),
        );
        assert_eq!(
            theme.paint(true, true, false, false),
            paint(
                colors.primary_background,
                colors.primary_foreground,
                colors.border_transparent
            )
        );
        assert_eq!(
            theme.paint(true, true, true, true),
            paint(
                colors.primary_pressed_background,
                colors.primary_pressed_foreground,
                colors.border_transparent
            )
        );
        // A disabled selection keeps a dimmed accent chip, apart from its enabled form and from
        // its disabled neighbours.
        let disabled = theme.paint(true, false, true, true);
        let accent = gpui_color(colors.primary_background);
        assert_eq!(
            (
                disabled.background().r,
                disabled.background().g,
                disabled.background().b
            ),
            (accent.r, accent.g, accent.b)
        );
        assert!(0.0 < disabled.background().a && disabled.background().a < accent.a);
        assert_eq!(disabled.border(), gpui_color(colors.border_transparent));
        assert_ne!(disabled, theme.paint(true, true, false, false));
        assert_ne!(
            disabled.background(),
            theme.paint(false, false, false, false).background()
        );
        // A card keeps its neutral fill and carries the accent as its ring.
        let card = theme.card_paint(true, true, false, false);
        assert_eq!(card.background(), gpui_color(colors.selection_background));
        assert_eq!(card.border(), gpui_color(colors.primary_background));
    }

    #[test]
    fn an_inactive_window_selects_in_neutral_grey_without_a_rim() {
        let colors = selection_colors();
        let theme = prepared(
            &colors,
            &ChromeTypography::default(),
            policy(false, false, false),
        );
        assert_eq!(
            theme.paint(true, true, false, false),
            paint(
                colors.selection_background,
                colors.selection_foreground,
                colors.border_transparent
            )
        );
        assert_eq!(
            theme.paint(true, false, false, false),
            paint(
                colors.selection_disabled_background,
                colors.selection_disabled_foreground,
                colors.border_transparent
            )
        );
        assert_eq!(
            theme.card_paint(true, true, false, false).border(),
            gpui_color(colors.border_selected)
        );

        let outlined = prepared(
            &colors,
            &ChromeTypography::default(),
            policy(false, false, true),
        );
        assert_eq!(
            outlined.paint(true, true, false, false).border(),
            gpui_color(colors.selection_border),
            "where the track keeps its edge, a neutral selection keeps one too"
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
        cx.set_global(theme(&ChromeColors::default()).scaled_spacing(1.25));
        let (_, cx) = cx.add_window_view(|_, _| Fixture);
        cx.run_until_parked();
        let option = cx.debug_bounds("geometry-option").expect("option paints");
        assert_eq!(option.size.height, px(27.0));
        assert_eq!(option.size.width, px(80.0));
        assert_eq!(
            cx.debug_bounds("geometry-option-label")
                .unwrap()
                .size
                .height,
            ChromeTypography::default()
                .style(TextRole::Body)
                .line_height
        );
    }

    #[test]
    fn selected_and_unselected_options_should_consume_distinct_roles() {
        let base = ChromeColors::default();
        let selected = ChromeColors {
            primary_background: Color::rgb(0x123456),
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
            primary_hover_background: Color::rgb(0x654322),
            ..base.clone()
        };

        let normal = theme(&base);
        for (changed, selected, hovered) in [
            (selected, true, false),
            (unselected_label, false, false),
            (hovered, false, true),
            (selected_hovered, true, true),
        ] {
            let changed = theme(&changed);
            assert_ne!(
                changed.paint(selected, true, hovered, false),
                normal.paint(selected, true, hovered, false)
            );
            assert_eq!(
                changed.paint(!selected, true, hovered, false),
                normal.paint(!selected, true, hovered, false)
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
        let theme = prepared(
            &colors,
            &ChromeTypography::default(),
            policy(true, true, true),
        );

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
            prepared(
                &colors,
                &ChromeTypography::default(),
                policy(true, false, true)
            ),
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

    #[gpui::test]
    fn only_a_track_that_keeps_its_edge_paints_a_border(cx: &mut gpui::TestAppContext) {
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
                        "edge-track",
                        "Mode",
                        &0,
                        vec![
                            spaceterm_ui::SegmentedOption::new(0, "Zero"),
                            spaceterm_ui::SegmentedOption::new(1, "One"),
                            spaceterm_ui::SegmentedOption::new(2, "Two")
                                .debug_selector("edge-track-two"),
                        ],
                    )
                    .unwrap()
                    .debug_selector("edge-track")
                    .on_change(|_, _, _| {}),
                )
            }
        }
        let colors = ChromeColors {
            border: Color::rgb(0x3c3c3c),
            element_border: Color::rgb(0x556677),
            ..ChromeColors::default()
        };
        // The track edge, when kept, is the separator role.
        for (edges, expected) in [
            (false, None),
            (true, Some(gpui::Hsla::from(gpui_color(colors.border)))),
        ] {
            cx.set_global(prepared(
                &colors,
                &ChromeTypography::default(),
                policy(true, false, edges),
            ));
            let (_, cx) = cx.add_window_view(|_, _| Fixture);
            cx.run_until_parked();
            let track = cx.debug_bounds("edge-track").expect("track paints");
            let divider = cx
                .debug_bounds("edge-track-two-divider")
                .expect("One and Two are unselected neighbours");
            cx.update(|window, _| {
                let quads = window.painted_quads();
                let track_edge = quads
                    .iter()
                    .find(|quad| {
                        quad.bounds == track.scale(window.scale_factor())
                            && quad.border_widths.left.0 > 0.0
                    })
                    .map(|quad| quad.border_color)
                    .filter(|color| color.a > 0.0);
                assert_eq!(track_edge, expected);
                let hairline = quads
                    .iter()
                    .find(|quad| quad.bounds == divider.scale(window.scale_factor()))
                    .expect("the divider paints");
                assert!(!hairline.background.is_transparent());
            });
        }
    }
}
