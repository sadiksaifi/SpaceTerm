use spaceterm_ui::ControlThemeCatalog;

use super::{
    button_theme, combo_box_theme, command_palette_theme, menu_theme, modal_theme,
    resize_handle_theme, scrollbar_theme, search_field_theme, segmented_control_theme,
    text_input_theme, toggle_theme, tooltip_theme,
};

pub(super) fn catalog(appearance: &super::appearance::ChromeAppearance) -> ControlThemeCatalog {
    // Controls paint the window's material. Overlay rows also receive the opaque presentation,
    // which stays the reference for every contrast decision.
    let reference = &appearance.colors;
    let colors = &appearance.control_colors;
    ControlThemeCatalog::new(
        button_theme::theme(colors),
        toggle_theme::theme(colors),
        scrollbar_theme::theme(colors),
        resize_handle_theme::theme(colors),
        segmented_control_theme::theme(colors),
        search_field_theme::themed(reference, colors),
        menu_theme::themed(reference, colors),
        command_palette_theme::themed(reference, colors),
        combo_box_theme::themed(reference, colors),
        text_input_theme::theme(colors),
        tooltip_theme::theme(colors),
        modal_theme::theme(colors),
    )
    .typography(spaceterm_ui::ControlTypography::new(
        appearance.regular.clone(),
        appearance.emphasis.clone(),
        appearance.heading.clone(),
    ))
    .scale_metrics(appearance.text_scale, appearance.spacing_scale)
}

/// One overlay row state in application colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OverlayRow {
    pub(super) fill: crate::appearance::Color,
    /// Foreground, secondary, icon and match colors, readable against the opaque reference.
    pub(super) content: [crate::appearance::Color; 4],
    pub(super) border: crate::appearance::Color,
}

impl OverlayRow {
    /// Resolves content against the opaque `reference` row over its surface, and paints the
    /// material `paint` row, so text never follows the window's changing fill alpha.
    pub(super) fn resolve(
        (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
        (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
        content: [crate::appearance::Color; 4],
        border: crate::appearance::Color,
    ) -> Self {
        let background = reference.source_over(reference_surface);
        Self {
            fill: row_fill((reference, reference_surface), (paint, paint_surface)),
            content: content.map(|color| readable_on(color, background, 4.5)),
            border,
        }
    }

    pub(super) fn paint(self) -> spaceterm_ui::ListRowPaint {
        let [foreground, secondary, icon, matched] = self.content;
        spaceterm_ui::ListRowPaint::new(
            gpui_color(self.fill),
            gpui_color(foreground),
            gpui_color(secondary),
            gpui_color(icon),
            gpui_color(matched),
            gpui_color(self.border),
        )
    }
}

pub(super) fn overlay_list_rows(
    reference: &crate::appearance::ChromeColors,
    paint: &crate::appearance::ChromeColors,
) -> spaceterm_ui::ListRowPaints {
    use spaceterm_ui::{ListRowPaint, ListRowPaints};
    let surfaces = (
        reference.elevated_surface_background,
        paint.elevated_surface_background,
    );
    let row = |pick: fn(&crate::appearance::ChromeColors) -> [crate::appearance::Color; 6]| {
        let [fill, foreground, secondary, icon, matched, border] = pick(reference);
        OverlayRow::resolve(
            (fill, surfaces.0),
            (pick(paint)[0], surfaces.1),
            [foreground, secondary, icon, matched],
            border,
        )
        .paint()
    };
    ListRowPaints::new(
        row(|c| {
            [
                c.elevated_surface_background,
                c.row_foreground,
                c.row_secondary,
                c.row_icon,
                c.row_match,
                c.row_border,
            ]
        }),
        row(|c| {
            [
                c.row_hover_background,
                c.row_hover_foreground,
                c.row_hover_secondary,
                c.row_hover_icon,
                c.row_hover_match,
                c.row_hover_border,
            ]
        }),
        row(|c| {
            [
                c.row_selected_background,
                c.row_selected_foreground,
                c.row_selected_secondary,
                c.row_selected_icon,
                c.row_selected_match,
                c.row_selected_border,
            ]
        }),
        row(|c| {
            [
                c.row_selected_hover_background,
                c.row_selected_hover_foreground,
                c.row_selected_hover_secondary,
                c.row_selected_hover_icon,
                c.row_selected_hover_match,
                c.row_selected_hover_border,
            ]
        }),
        ListRowPaint::new(
            gpui_color(if surfaces.0 == surfaces.1 {
                surfaces.1
            } else {
                crate::appearance::Color::rgba(0)
            }),
            gpui_color(reference.text_disabled),
            gpui_color(reference.text_disabled),
            gpui_color(reference.icon_disabled),
            gpui_color(reference.text_disabled),
            gpui_color(reference.row_border),
        ),
    )
}

/// What a row paints over the surface it rests on. Without a material it paints its composite,
/// as authored. With a material it paints only its own fill, and an idle row paints nothing, so
/// the surface is never composited twice.
pub(super) fn row_fill(
    (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
    (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
) -> crate::appearance::Color {
    if paint == reference && paint_surface == reference_surface {
        reference.source_over(reference_surface)
    } else if reference == reference_surface {
        crate::appearance::Color::rgba(0)
    } else {
        paint
    }
}

pub(super) fn readable_on(
    proposed: crate::appearance::Color,
    background: crate::appearance::Color,
    minimum_contrast: f64,
) -> crate::appearance::Color {
    let rendered = proposed.source_over(background);
    if rendered.contrast_ratio(background) >= minimum_contrast {
        return rendered;
    }

    let dark = crate::appearance::Color::rgb(0x000000);
    let light = crate::appearance::Color::rgb(0xffffff);
    let target = if dark.contrast_ratio(background) >= light.contrast_ratio(background) {
        dark
    } else {
        light
    };
    let mut lower = 0.0;
    let mut upper = 1.0;
    let mut readable = target;
    for _ in 0..16 {
        let amount = (lower + upper) / 2.0;
        let candidate = rendered.mix(target, amount);
        if candidate.contrast_ratio(background) >= minimum_contrast {
            readable = candidate;
            upper = amount;
        } else {
            lower = amount;
        }
    }
    readable
}

fn gpui_color(color: crate::appearance::Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{ChromeColors, Color};

    /// Row content resolves against the opaque presentation, so a translucent window never moves
    /// text or icons, including Light at full transparency where every fill has no alpha.
    #[test]
    fn overlay_row_content_stays_stable_across_transparency_in_both_appearances() {
        use crate::appearance::{
            Appearance, AppearancePreferences, ResolvedWindowComposition, builtin_chrome_base,
        };
        for appearance in [Appearance::Light, Appearance::Dark] {
            let reference = builtin_chrome_base(appearance).opaque_presentation();
            let rows_at = |transparency: f32| {
                let mut preferences = AppearancePreferences::default();
                preferences.background.transparency = transparency;
                let materials =
                    ResolvedWindowComposition::resolve(&preferences.background, true).materials;
                let paint = reference.material_presentation(materials);
                (overlay_list_rows(&reference, &paint), paint)
            };
            type RowColors = fn(&ChromeColors) -> [Color; 6];
            let states: [(bool, bool, RowColors); 3] = [
                (false, false, |c| {
                    [
                        c.elevated_surface_background,
                        c.row_foreground,
                        c.row_secondary,
                        c.row_icon,
                        c.row_match,
                        c.row_border,
                    ]
                }),
                (false, true, |c| {
                    [
                        c.row_hover_background,
                        c.row_hover_foreground,
                        c.row_hover_secondary,
                        c.row_hover_icon,
                        c.row_hover_match,
                        c.row_hover_border,
                    ]
                }),
                (true, false, |c| {
                    [
                        c.row_selected_background,
                        c.row_selected_foreground,
                        c.row_selected_secondary,
                        c.row_selected_icon,
                        c.row_selected_match,
                        c.row_selected_border,
                    ]
                }),
            ];
            let (opaque_rows, _) = rows_at(0.0);
            for transparency in [0.15, 1.0] {
                let (rows, paint) = rows_at(transparency);
                for (selected, hovered, pick) in states {
                    let [fill, foreground, secondary, icon, matched, border] = pick(&reference);
                    let opaque = OverlayRow::resolve(
                        (fill, reference.elevated_surface_background),
                        (fill, reference.elevated_surface_background),
                        [foreground, secondary, icon, matched],
                        border,
                    );
                    assert_eq!(
                        opaque_rows.resolve(true, selected, hovered),
                        opaque.paint(),
                        "{appearance:?} opaque rows keep their composite paint"
                    );
                    let expected = OverlayRow {
                        fill: row_fill(
                            (fill, reference.elevated_surface_background),
                            (pick(&paint)[0], paint.elevated_surface_background),
                        ),
                        content: opaque.content,
                        border,
                    };
                    assert_eq!(
                        rows.resolve(true, selected, hovered),
                        expected.paint(),
                        "{appearance:?} at {transparency}: row content must not follow fill alpha"
                    );
                    if transparency == 1.0 {
                        // The maximum setting keeps a faint fill rather than none, so a row
                        // that states a state still states it. A row that matches the surface
                        // it rests on still paints nothing at all.
                        assert!(
                            expected.fill.a < 128,
                            "{appearance:?}: a resting row must still transmit most of its backing: {:?}",
                            expected.fill
                        );
                        assert_eq!(
                            selected || hovered,
                            expected.fill.a > 0,
                            "{appearance:?}: {:?}",
                            expected.fill
                        );
                    }
                }
            }
            if appearance == Appearance::Light {
                let [_, foreground, ..] = (states[0].2)(&reference);
                let readable = readable_on(foreground, reference.elevated_surface_background, 4.5);
                assert!(
                    readable.contrast_ratio(Color::rgb(0x000000))
                        < readable.contrast_ratio(Color::rgb(0xffffff)),
                    "Light overlay text stays dark at every transparency"
                );
            }
        }
    }

    #[test]
    fn overlay_rows_preserve_interaction_states_but_inherit_the_elevated_idle_surface() {
        use gpui::rgba;
        use spaceterm_ui::ListRowPaint;
        let colors = ChromeColors {
            row_background: Color::rgba(0x10203040),
            row_foreground: Color::rgba(0x14233142),
            row_secondary: Color::rgba(0x18263244),
            row_icon: Color::rgba(0x1c293346),
            row_match: Color::rgba(0x202c3448),
            row_border: Color::rgba(0x242f354a),
            row_hover_background: Color::rgba(0x2832364c),
            row_hover_foreground: Color::rgba(0x2c35374e),
            row_hover_secondary: Color::rgba(0x30383850),
            row_hover_icon: Color::rgba(0x343b3952),
            row_hover_match: Color::rgba(0x383e3a54),
            row_hover_border: Color::rgba(0x3c413b56),
            row_selected_background: Color::rgba(0x40443c58),
            row_selected_foreground: Color::rgba(0x44473d5a),
            row_selected_secondary: Color::rgba(0x484a3e5c),
            row_selected_icon: Color::rgba(0x4c4d3f5e),
            row_selected_match: Color::rgba(0x50504060),
            row_selected_border: Color::rgba(0x54534162),
            row_selected_hover_background: Color::rgba(0x58564264),
            row_selected_hover_foreground: Color::rgba(0x5c594366),
            row_selected_hover_secondary: Color::rgba(0x605c4468),
            row_selected_hover_icon: Color::rgba(0x645f456a),
            row_selected_hover_match: Color::rgba(0x6862466c),
            row_selected_hover_border: Color::rgba(0x6c65476e),
            ..ChromeColors::default()
        };
        let rows = overlay_list_rows(&colors, &colors);
        let expected = |background: Color,
                        foreground: Color,
                        secondary: Color,
                        icon: Color,
                        matched: Color,
                        border: Color| {
            let background = background.source_over(colors.elevated_surface_background);
            ListRowPaint::new(
                rgba(background.rgba_hex()),
                rgba(readable_on(foreground, background, 4.5).rgba_hex()),
                rgba(readable_on(secondary, background, 4.5).rgba_hex()),
                rgba(readable_on(icon, background, 4.5).rgba_hex()),
                rgba(readable_on(matched, background, 4.5).rgba_hex()),
                rgba(border.rgba_hex()),
            )
        };
        assert_eq!(
            rows.resolve(true, false, false),
            expected(
                colors.elevated_surface_background,
                colors.row_foreground,
                colors.row_secondary,
                colors.row_icon,
                colors.row_match,
                colors.row_border,
            )
        );
        assert_eq!(
            rows.resolve(true, false, true),
            expected(
                colors.row_hover_background,
                colors.row_hover_foreground,
                colors.row_hover_secondary,
                colors.row_hover_icon,
                colors.row_hover_match,
                colors.row_hover_border,
            )
        );
        assert_eq!(
            rows.resolve(true, true, false),
            expected(
                colors.row_selected_background,
                colors.row_selected_foreground,
                colors.row_selected_secondary,
                colors.row_selected_icon,
                colors.row_selected_match,
                colors.row_selected_border,
            )
        );
        assert_eq!(
            rows.resolve(true, true, true),
            expected(
                colors.row_selected_hover_background,
                colors.row_selected_hover_foreground,
                colors.row_selected_hover_secondary,
                colors.row_selected_hover_icon,
                colors.row_selected_hover_match,
                colors.row_selected_hover_border,
            )
        );
        assert_eq!(
            rows.resolve(false, true, true),
            ListRowPaint::new(
                rgba(colors.elevated_surface_background.rgba_hex()),
                rgba(colors.text_disabled.rgba_hex()),
                rgba(colors.text_disabled.rgba_hex()),
                rgba(colors.icon_disabled.rgba_hex()),
                rgba(colors.text_disabled.rgba_hex()),
                rgba(colors.row_border.rgba_hex()),
            )
        );
    }

    #[test]
    fn list_themes_should_consume_hover_and_selection_independently() {
        let base = ChromeColors::default();
        let hovered = ChromeColors {
            row_hover_background: Color::rgb(0x123456),
            ..base.clone()
        };
        let selected = ChromeColors {
            row_selected_background: Color::rgb(0xabcdef),
            ..base.clone()
        };
        let hover_foreground = ChromeColors {
            row_hover_foreground: Color::rgb(0x123456),
            ..base.clone()
        };
        let selected_foreground = ChromeColors {
            row_selected_foreground: Color::rgb(0xabcdef),
            ..base.clone()
        };
        for changed in [hovered, selected, hover_foreground, selected_foreground] {
            assert_ne!(menu_theme::theme(&base), menu_theme::theme(&changed));
            assert_ne!(
                combo_box_theme::theme(&base),
                combo_box_theme::theme(&changed)
            );
            assert_ne!(
                command_palette_theme::theme(&base),
                command_palette_theme::theme(&changed)
            );
        }
    }

    #[test]
    fn overlay_controls_keep_idle_rows_on_their_raised_panel() {
        let base = ChromeColors::default().opaque_presentation();
        let changed_shell_row = ChromeColors {
            row_background: Color::rgb(0xff00ff),
            ..base.clone()
        };
        let changed_overlay = ChromeColors {
            elevated_surface_background: Color::rgb(0x004488),
            ..base.clone()
        };

        assert_eq!(
            menu_theme::theme(&base),
            menu_theme::theme(&changed_shell_row)
        );
        assert_eq!(
            combo_box_theme::theme(&base),
            combo_box_theme::theme(&changed_shell_row)
        );
        assert_eq!(
            command_palette_theme::theme(&base),
            command_palette_theme::theme(&changed_shell_row)
        );
        assert_ne!(
            menu_theme::theme(&base),
            menu_theme::theme(&changed_overlay)
        );
        assert_ne!(
            combo_box_theme::theme(&base),
            combo_box_theme::theme(&changed_overlay)
        );
        assert_ne!(
            command_palette_theme::theme(&base),
            command_palette_theme::theme(&changed_overlay)
        );
    }
}
