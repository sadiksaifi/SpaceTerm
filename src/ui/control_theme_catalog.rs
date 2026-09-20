use spaceterm_ui::ControlThemeCatalog;

use super::{
    button_theme, combo_box_theme, command_palette_theme, menu_theme, modal_theme, progress_theme,
    resize_handle_theme, scrollbar_theme, search_field_theme, segmented_control_theme,
    text_input_theme, toggle_theme, tooltip_theme,
};

pub(super) fn catalog(
    appearance: &super::appearance::ChromeAppearance,
    progress_motion: spaceterm_ui::ProgressMotion,
) -> ControlThemeCatalog {
    // Controls paint the window's material. Floating controls use the same semantic colors as
    // their host, with fills compiled into overlays so the shell remains visible beneath them.
    // Overlay rows also receive the opaque presentation as their contrast reference.
    let reference = &appearance.colors;
    let colors = &appearance.control_colors;
    let segmented = &appearance.segmented_control_colors;
    let host = &appearance.floating_colors;
    let floating_controls = &appearance.floating_control_colors;
    let floating_segmented = &appearance.floating_segmented_colors;
    let field_reference = &appearance.floating_field_reference;
    let field = &appearance.floating_field_colors;
    // The shell applies its backdrop tone and elevation wash once. Idle rows inherit that host,
    // while row states keep their complete host-relative paints and content contrast reference.
    let mut popup = host.clone();
    popup.elevated_surface_background =
        appearance.floating_surface(appearance.colors.elevated_surface_background);
    let row_reference = host.clone();
    let panel_controls = surface_control_themes(
        &appearance.panel_controls,
        &row_reference,
        &popup,
        progress_motion,
    );
    let card_controls = surface_control_themes(
        &appearance.card_controls,
        &row_reference,
        &popup,
        progress_motion,
    );
    ControlThemeCatalog::new(
        button_theme::theme(colors),
        toggle_theme::theme(colors),
        progress_theme::theme(colors, progress_motion),
        scrollbar_theme::theme(colors),
        resize_handle_theme::theme(colors),
        segmented_control_theme::theme(segmented),
        search_field_theme::themed(reference, colors),
        menu_theme::themed_with_rows(&row_reference, colors, &popup),
        command_palette_theme::themed(&row_reference, &popup),
        combo_box_theme::themed_with_rows(&row_reference, colors, &popup),
        text_input_theme::theme(colors),
        tooltip_theme::theme(host),
        modal_theme::theme(floating_controls),
    )
    .resting_controls(panel_controls, card_controls)
    .floating(
        appearance.floating_surfaces(),
        spaceterm_ui::SurfaceControlThemes::new(
            button_theme::theme(floating_controls),
            toggle_theme::theme(floating_controls),
            progress_theme::theme(floating_controls, progress_motion),
            segmented_control_theme::theme(floating_segmented),
            search_field_theme::themed(field_reference, field),
            text_input_theme::themed(field, host),
        )
        .triggers(
            menu_theme::themed_with_rows(&row_reference, floating_controls, &popup),
            combo_box_theme::themed_with_rows(&row_reference, floating_controls, &popup),
        ),
    )
    .typography(spaceterm_ui::ControlTypography::new(
        appearance.regular.clone(),
        appearance.emphasis.clone(),
        appearance.heading.clone(),
    ))
    .scale_metrics(appearance.text_scale, appearance.spacing_scale)
}

pub(super) fn surface_control_themes(
    host: &super::appearance::PreparedControlHost,
    row_reference: &crate::appearance::ChromeColors,
    popup: &crate::appearance::ChromeColors,
    progress_motion: spaceterm_ui::ProgressMotion,
) -> spaceterm_ui::SurfaceControlThemes {
    spaceterm_ui::SurfaceControlThemes::new(
        button_theme::theme(&host.colors),
        toggle_theme::theme(&host.colors),
        progress_theme::theme(&host.colors, progress_motion),
        segmented_control_theme::theme(&host.segmented),
        search_field_theme::themed(&host.reference, &host.colors),
        text_input_theme::theme(&host.colors),
    )
    .triggers(
        menu_theme::themed_with_rows(row_reference, &host.colors, popup),
        combo_box_theme::themed_with_rows(row_reference, &host.colors, popup),
    )
}

/// One overlay row state in application colors.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct OverlayRow {
    pub(super) fill: crate::appearance::Color,
    /// Foreground, secondary, icon and match colors, readable across the final backdrop endpoints.
    pub(super) content: [crate::appearance::Color; 4],
    pub(super) border: crate::appearance::Color,
}

impl OverlayRow {
    /// Resolves content against both admitted material endpoints and paints the smallest
    /// host-relative row state that can carry that content.
    pub(super) fn resolve(
        (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
        (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
        content: [crate::appearance::Color; 4],
        border: crate::appearance::Color,
    ) -> Self {
        let mut fill = row_fill((reference, reference_surface), (paint, paint_surface));
        if !paint_surface.is_opaque()
            && let Some(compressed) = readable_material_row_fill(fill, paint_surface)
        {
            fill = compressed;
        }
        let mut backgrounds = row_backgrounds(fill, paint_surface);
        if shared_neutral(backgrounds).is_none() {
            fill = reference.source_over(reference_surface);
            backgrounds = [fill; 2];
        }
        Self {
            fill,
            content: content
                .map(|color| super::appearance::readable_on_backgrounds(color, backgrounds, 4.5)),
            border: if paint_surface == reference_surface || fill.is_opaque() {
                border
            } else {
                relative_edge(reference.source_over(reference_surface), border)
            },
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

fn row_backgrounds(
    fill: crate::appearance::Color,
    surface: crate::appearance::Color,
) -> [crate::appearance::Color; 2] {
    if surface.is_opaque() {
        return [fill.source_over(surface); 2];
    }
    [
        crate::appearance::Color::rgb(0x000000),
        crate::appearance::Color::rgb(0xffffff),
    ]
    .map(|underlay| fill.source_over(surface.source_over(underlay)))
}

fn shared_neutral(backgrounds: [crate::appearance::Color; 2]) -> Option<crate::appearance::Color> {
    let score = |foreground: crate::appearance::Color| {
        backgrounds
            .into_iter()
            .map(|background| foreground.contrast_ratio(background))
            .fold(f64::INFINITY, f64::min)
    };
    let dark = crate::appearance::Color::rgb(0x000000);
    let light = crate::appearance::Color::rgb(0xffffff);
    let (foreground, contrast) = if score(dark) >= score(light) {
        (dark, score(dark))
    } else {
        (light, score(light))
    };
    (contrast >= 4.5).then_some(foreground)
}

/// Compresses one row-state overlay into the readable interval connected to transparent.
///
/// Fully opaque ink can become readable again after crossing an inaccessible middle interval;
/// stopping at the first failure keeps the result on the material side of that interval. The
/// compression curve preserves ordering among states that share the same ink and host instead of
/// flattening each of them onto one alpha ceiling.
fn readable_material_row_fill(
    fill: crate::appearance::Color,
    surface: crate::appearance::Color,
) -> Option<crate::appearance::Color> {
    let foreground = shared_neutral(row_backgrounds(fill.with_alpha(0), surface))?;
    if fill.a == 0 {
        return Some(fill);
    }
    let ink = fill.with_alpha(255);
    let mut ceiling = 0_u8;
    for alpha in 1..=u8::MAX {
        let backgrounds = row_backgrounds(ink.with_alpha(alpha), surface);
        if backgrounds
            .into_iter()
            .all(|background| foreground.contrast_ratio(background) >= 4.5)
        {
            ceiling = alpha;
        } else {
            break;
        }
    }
    if ceiling == 0 {
        return None;
    }
    let ceiling = f64::from(ceiling) / 255.0;
    let alpha = f64::from(fill.a) / 255.0;
    let compressed = ceiling * alpha * (1.0 + ceiling) / (alpha + ceiling);
    Some(fill.with_alpha((compressed.clamp(0.0, ceiling) * 255.0).round() as u8))
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
            gpui_color(if surfaces.0 == surfaces.1 {
                reference.row_border
            } else {
                relative_edge(surfaces.0, reference.row_border)
            }),
        ),
    )
}

/// What a row paints over the surface it rests on. Without a material it paints its authored
/// composite. With a material it paints the smallest host-relative overlay that reaches the same
/// state, and an idle row paints nothing, so the surface is never composited twice.
pub(super) fn row_fill(
    (reference, reference_surface): (crate::appearance::Color, crate::appearance::Color),
    (paint, paint_surface): (crate::appearance::Color, crate::appearance::Color),
) -> crate::appearance::Color {
    if paint == reference && paint_surface == reference_surface {
        reference.source_over(reference_surface)
    } else if reference == reference_surface {
        crate::appearance::Color::rgba(0)
    } else {
        let target = reference.source_over(reference_surface);
        let base = if paint_surface.is_opaque() {
            paint_surface
        } else {
            reference_surface
        };
        target.relative_overlay(base)
    }
}

/// Reconstructs an authored edge over its semantic host for a translucent row.
pub(super) fn relative_edge(
    host: crate::appearance::Color,
    edge: crate::appearance::Color,
) -> crate::appearance::Color {
    if edge.a == 0 {
        return edge;
    }
    edge.source_over(host).relative_overlay(host)
}

pub(super) fn readable_on(
    proposed: crate::appearance::Color,
    background: crate::appearance::Color,
    minimum_contrast: f64,
) -> crate::appearance::Color {
    super::appearance::readable_on_background(proposed, background, minimum_contrast)
}

fn gpui_color(color: crate::appearance::Color) -> gpui::Rgba {
    gpui::rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{ChromeColors, Color};

    #[test]
    fn material_row_fill_reconstructs_the_authored_state_on_an_opaque_host() {
        let surface = Color::rgb(0x202020);
        let state = Color::rgb(0x262626);
        let actual_host = Color::rgb(0x181818);
        let fill = row_fill((state, surface), (state, actual_host));
        let rendered = fill.source_over(actual_host);

        assert_eq!(rendered, state);
        assert!(
            fill.a < 32,
            "a small elevation step needs only a thin overlay"
        );
    }

    #[test]
    fn material_row_fill_preserves_a_custom_chromatic_state_as_a_relative_overlay() {
        let surface = Color::rgb(0x202020);
        let state = Color::rgb(0x603028);
        let row = OverlayRow::resolve(
            (state, surface),
            (state, Color::rgba(0x202020b3)),
            [Color::rgb(0xffffff); 4],
            Color::rgba(0),
        );
        let fill = row.fill;

        assert!(fill.a > 0 && fill.a < 128);
        assert!(
            fill.r > fill.g && fill.r > fill.b,
            "the red authored direction must survive host-relative reconstruction: {fill:?}"
        );
    }

    #[test]
    fn dark_selected_row_lifts_from_its_semantic_host_even_when_material_rgb_is_lighter() {
        let surface = Color::rgb(0x202020);
        let selected = Color::rgb(0x262626);
        let combined_material = Color::rgba(0x363636b9);
        let fill = row_fill((selected, surface), (selected, combined_material));

        assert!(fill.a > 0 && fill.a < 32);
        assert!(
            fill.r >= 250 && fill.g >= 250 && fill.b >= 250,
            "selected must remain a lightening step over the native-backed host: {fill:?}"
        );
    }

    #[test]
    fn row_with_no_shared_readable_foreground_falls_back_once_to_opaque_state() {
        let middle = Color::rgb(0x808080);
        let row = OverlayRow::resolve(
            (middle, middle),
            (Color::rgba(0), Color::rgba(0)),
            [Color::rgb(0x777777); 4],
            Color::rgba(0),
        );

        assert_eq!(row.fill, middle);
        assert!(
            row.content
                .into_iter()
                .all(|content| content.contrast_ratio(middle) >= 4.5)
        );
    }

    #[test]
    fn nonzero_row_state_uses_fallback_when_no_nonzero_readable_alpha_exists() {
        let surface = Color::rgba(0x00000089);
        let fill = Color::rgba(0xffffff10);

        assert!(shared_neutral(row_backgrounds(fill.with_alpha(0), surface)).is_some());
        assert!(
            readable_material_row_fill(fill, surface).is_none(),
            "a requested state must not disappear when its readable interval has zero width"
        );
    }

    /// Built-in row content stays authored when possible and remains readable across materials.
    #[test]
    fn overlay_row_content_stays_readable_across_transparency_in_both_appearances() {
        use crate::appearance::{
            Appearance, AppearancePreferences, ResolvedWindowComposition, builtin_chrome_base,
        };
        for appearance in [Appearance::Light, Appearance::Dark] {
            let reference = builtin_chrome_base(appearance).opaque_presentation();
            let rows_at = |transparency: f32| {
                let mut preferences = AppearancePreferences::default();
                preferences.background.transparency = transparency;
                let materials = ResolvedWindowComposition::resolve(
                    &preferences.background,
                    crate::appearance::CompositionCapabilities::new(true, true),
                )
                .materials;
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
                    let expected = OverlayRow::resolve(
                        (fill, reference.elevated_surface_background),
                        (pick(&paint)[0], paint.elevated_surface_background),
                        [foreground, secondary, icon, matched],
                        border,
                    );
                    assert_eq!(
                        rows.resolve(true, selected, hovered),
                        expected.paint(),
                        "{appearance:?} at {transparency}: rows use the final material endpoints"
                    );
                    let backgrounds = if paint.elevated_surface_background.is_opaque() {
                        [expected.fill.source_over(paint.elevated_surface_background); 2]
                    } else {
                        [Color::rgb(0x000000), Color::rgb(0xffffff)].map(|underlay| {
                            expected.fill.source_over(
                                paint.elevated_surface_background.source_over(underlay),
                            )
                        })
                    };
                    for (authored, resolved) in [foreground, secondary, icon, matched]
                        .into_iter()
                        .zip(expected.content)
                    {
                        assert!(
                            backgrounds.into_iter().all(|background| resolved
                                .source_over(background)
                                .contrast_ratio(background)
                                >= 4.5),
                            "{appearance:?} at {transparency}: {resolved:?} must read over {backgrounds:?}"
                        );
                        if backgrounds.into_iter().all(|background| {
                            authored.source_over(background).contrast_ratio(background) >= 4.5
                        }) {
                            assert_eq!(
                                resolved, authored,
                                "{appearance:?} at {transparency}: readable authored content stays exact"
                            );
                        }
                    }
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
    fn built_in_floating_row_states_remain_distinct_translucent_and_readable_at_maximum_glass() {
        use crate::appearance::{
            Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences,
            AvailableFonts, CompositionCapabilities, SchemeCatalog, SystemAppearance,
        };
        use crate::ui::appearance::ChromeAppearance;

        for appearance in [Appearance::Light, Appearance::Dark] {
            let mut preferences = AppearancePreferences {
                mode: match appearance {
                    Appearance::Light => AppearanceMode::Light,
                    Appearance::Dark => AppearanceMode::Dark,
                },
                ..AppearancePreferences::default()
            };
            preferences.background.transparency = 1.0;
            let resolved = SchemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::available(appearance)
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .expect("built-in appearance should resolve");
            let prepared = ChromeAppearance::prepare(&resolved.chrome);
            let reference = &prepared.floating_colors;
            let material = prepared.floating_surface(prepared.colors.elevated_surface_background);
            let mut paint = reference.clone();
            paint.elevated_surface_background = material;
            type RowColors = fn(&ChromeColors) -> [Color; 6];
            let states: [RowColors; 3] = [
                |c| {
                    [
                        c.row_hover_background,
                        c.row_hover_foreground,
                        c.row_hover_secondary,
                        c.row_hover_icon,
                        c.row_hover_match,
                        c.row_hover_border,
                    ]
                },
                |c| {
                    [
                        c.row_selected_background,
                        c.row_selected_foreground,
                        c.row_selected_secondary,
                        c.row_selected_icon,
                        c.row_selected_match,
                        c.row_selected_border,
                    ]
                },
                |c| {
                    [
                        c.row_selected_hover_background,
                        c.row_selected_hover_foreground,
                        c.row_selected_hover_secondary,
                        c.row_selected_hover_icon,
                        c.row_selected_hover_match,
                        c.row_selected_hover_border,
                    ]
                },
            ];
            let rows = states.map(|pick| {
                let [fill, foreground, secondary, icon, matched, border] = pick(reference);
                OverlayRow::resolve(
                    (fill, reference.elevated_surface_background),
                    (pick(&paint)[0], paint.elevated_surface_background),
                    [foreground, secondary, icon, matched],
                    border,
                )
            });

            for row in rows {
                assert!(
                    row.fill.a > 0 && row.fill.a < 255,
                    "{appearance:?} row state must remain a translucent material: {:?}",
                    row.fill
                );
                for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                    let background = row.fill.source_over(material.source_over(underlay));
                    assert!(
                        row.content
                            .into_iter()
                            .all(|content| content.contrast_ratio(background) >= 4.5),
                        "{appearance:?} row content must read over {background:?}"
                    );
                }
            }
            let alphas = rows.map(|row| row.fill.a);
            assert!(
                alphas.windows(2).all(|pair| pair[0] < pair[1]),
                "{appearance:?} row states must retain their authored order: {alphas:?}"
            );
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
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(
                        foreground,
                        [background; 2],
                        4.5,
                    )
                    .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(secondary, [background; 2], 4.5)
                        .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(icon, [background; 2], 4.5)
                        .rgba_hex(),
                ),
                rgba(
                    crate::ui::appearance::readable_on_backgrounds(matched, [background; 2], 4.5)
                        .rgba_hex(),
                ),
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
