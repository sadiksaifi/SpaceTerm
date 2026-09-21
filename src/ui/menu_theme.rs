use gpui::{Rgba, px, rgba};
use spaceterm_ui::{MenuMetrics, MenuPaint, MenuSizes, MenuTheme};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_geometry::{HAIRLINE, RadiusRole};
use crate::ui::chrome_icons::{ChromeIcons, IconRole};
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

#[cfg(test)]
pub(super) fn theme(colors: &ChromeColors) -> MenuTheme {
    themed_with_rows(colors, colors, colors)
}

/// Keeps a trigger's host treatment independent from rows on the shared floating material.
#[cfg(test)]
pub(super) fn themed_with_rows(
    reference: &ChromeColors,
    colors: &ChromeColors,
    row_colors: &ChromeColors,
) -> MenuTheme {
    prepared_with_rows(
        reference,
        colors,
        row_colors,
        None,
        &ChromeTypography::default(),
        &ChromeIcons::default(),
        super::control_theme_catalog::OverlayRowPolicy::default(),
    )
}

pub(super) fn prepared_with_rows(
    reference: &ChromeColors,
    colors: &ChromeColors,
    row_colors: &ChromeColors,
    unfocused_row_colors: Option<&ChromeColors>,
    typography: &ChromeTypography,
    icons: &ChromeIcons,
    row_policy: super::control_theme_catalog::OverlayRowPolicy,
) -> MenuTheme {
    let rows = super::control_theme_catalog::overlay_list_rows_with_policy(
        reference, row_colors, row_policy,
    );
    let rows = unfocused_row_colors.map_or(rows, |unfocused| {
        rows.unfocused_selection(super::control_theme_catalog::overlay_list_rows_with_policy(
            unfocused, unfocused, row_policy,
        ))
    });
    let destructive = destructive_rows(reference, row_colors, row_policy);
    let destructive = unfocused_row_colors.map_or(destructive, |unfocused| {
        destructive.unfocused_selection(destructive_rows(unfocused, unfocused, row_policy))
    });
    let paint = MenuPaint::new(
        gpui_color(colors.text),
        gpui_color(colors.icon),
        gpui_color(colors.text_disabled),
        gpui_color(colors.ghost_element_selected),
        gpui_color(colors.ghost_element_selected_foreground),
        gpui_color(colors.error),
    )
    .rows(rows)
    .destructive_rows(destructive)
    .hover_background(gpui_color(colors.ghost_element_hover))
    .hover_foreground(gpui_color(colors.ghost_element_hover_foreground))
    .trigger(
        gpui_color(colors.ghost_element_background),
        gpui_color(colors.ghost_element_hover),
        gpui_color(colors.border_transparent),
    )
    .focus_border(gpui_color(colors.focus_ring));

    MenuTheme::new(
        paint,
        MenuSizes::new(
            metrics(200.0, typography, icons),
            metrics(224.0, typography, icons),
            metrics(264.0, typography, icons),
        ),
    )
}

fn destructive_rows(
    reference: &ChromeColors,
    colors: &ChromeColors,
    policy: super::control_theme_catalog::OverlayRowPolicy,
) -> spaceterm_ui::ListRowPaints {
    let mut destructive = reference.clone();
    macro_rules! error_content {
        ($($field:ident),+ $(,)?) => { $(destructive.$field = reference.error;)+ };
    }
    error_content!(
        row_foreground,
        row_secondary,
        row_icon,
        row_match,
        row_hover_foreground,
        row_hover_secondary,
        row_hover_icon,
        row_hover_match,
        row_selected_foreground,
        row_selected_secondary,
        row_selected_icon,
        row_selected_match,
        row_selected_hover_foreground,
        row_selected_hover_secondary,
        row_selected_hover_icon,
        row_selected_hover_match
    );
    super::control_theme_catalog::overlay_list_rows_with_policy(&destructive, colors, policy)
}

fn metrics(width: f32, typography: &ChromeTypography, icons: &ChromeIcons) -> MenuMetrics {
    let body = typography.style(TextRole::Body);
    let shortcut = typography.style(TextRole::Shortcut);
    let section = typography.style(TextRole::Section);
    MenuMetrics::new(px(width), px(26.0))
        .trigger_height(px(28.0))
        .horizontal_padding(px(6.0))
        .indicator_width(px(16.0))
        .gap(px(6.0))
        .trigger_corner_radius(RadiusRole::Control.pixels())
        .font_sizes(body.size, shortcut.size)
        .section_font_size(section.size)
        .submenu_gap(px(2.0))
        .decoration_metrics(icons.metrics(IconRole::Row).glyph_size, px(HAIRLINE))
        .trigger_icon_size(icons.metrics(IconRole::Control).glyph_size)
        .text_geometry(
            body.line_height,
            shortcut.line_height,
            section.line_height,
            icons.metrics(IconRole::Row).baseline_center,
        )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{Appearance, builtin_chrome_base};
    use spaceterm_ui::ListRowPaint;

    fn expected_row(
        colors: &ChromeColors,
        background: Color,
        foreground: Color,
        icon: Color,
        border: Color,
    ) -> ListRowPaint {
        let background = background.source_over(colors.elevated_surface_background);
        let foreground =
            super::super::control_theme_catalog::readable_on(foreground, background, 4.5);
        let icon = super::super::control_theme_catalog::readable_on(icon, background, 4.5);
        ListRowPaint::new(
            gpui_color(background),
            gpui_color(foreground),
            gpui_color(foreground),
            gpui_color(icon),
            gpui_color(foreground),
            gpui_color(border),
        )
    }

    #[test]
    fn destructive_menu_rows_use_neutral_overlay_surfaces_and_semantic_content() {
        for appearance in [Appearance::Dark, Appearance::Light] {
            let colors = builtin_chrome_base(appearance).opaque_presentation();
            let rows = destructive_rows(
                &colors,
                &colors,
                super::super::control_theme_catalog::OverlayRowPolicy::default(),
            );

            assert_eq!(
                rows.resolve(true, false, false),
                expected_row(
                    &colors,
                    colors.elevated_surface_background,
                    colors.error,
                    colors.error,
                    colors.row_border,
                )
            );
            assert_eq!(
                rows.resolve(true, false, true),
                expected_row(
                    &colors,
                    colors.row_hover_background,
                    colors.error,
                    colors.error,
                    colors.row_hover_border,
                )
            );
            assert_eq!(
                rows.resolve(true, true, false),
                expected_row(
                    &colors,
                    colors.row_selected_background,
                    colors.error,
                    colors.error,
                    colors.row_selected_border,
                )
            );
            assert_eq!(
                rows.resolve(true, true, true),
                expected_row(
                    &colors,
                    colors.row_selected_hover_background,
                    colors.error,
                    colors.error,
                    colors.row_selected_hover_border,
                )
            );
            assert_eq!(
                rows.resolve(false, true, true),
                expected_row(
                    &colors,
                    colors.row_selected_background,
                    colors.text_disabled,
                    colors.icon_disabled,
                    colors.row_selected_border,
                )
            );
            assert_ne!(
                rows.resolve(false, true, true),
                rows.resolve(false, false, true),
                "disabling a destructive row must preserve its selection"
            );
            assert_ne!(
                rows.resolve(true, false, false),
                expected_row(
                    &colors,
                    colors.destructive_background,
                    colors.destructive_foreground,
                    colors.destructive_icon,
                    colors.destructive_border,
                ),
                "an idle destructive menu action must not look like a filled button"
            );
        }
    }
}
