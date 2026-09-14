use gpui::{Rgba, px, rgba};
use spaceterm_ui::{MenuMetrics, MenuPaint, MenuSizes, MenuTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> MenuTheme {
    let paint = MenuPaint::new(
        gpui_color(colors.elevated_surface_background),
        gpui_color(colors.border),
        gpui_color(colors.text),
        gpui_color(colors.icon),
        gpui_color(colors.text_disabled),
        gpui_color(colors.ghost_element_selected),
        gpui_color(colors.ghost_element_selected_foreground),
        gpui_color(colors.error),
        gpui_color(colors.border),
    )
    .rows(super::control_theme_catalog::overlay_list_rows(colors))
    .destructive_rows(destructive_rows(colors))
    .hover_background(gpui_color(colors.ghost_element_hover))
    .hover_foreground(gpui_color(colors.ghost_element_hover_foreground))
    .trigger(
        gpui_color(colors.ghost_element_background),
        gpui_color(colors.ghost_element_hover),
        gpui_color(colors.border_transparent),
    )
    .focus_border(gpui_color(colors.border_focused));

    MenuTheme::new(
        paint,
        MenuSizes::new(metrics(196.0), metrics(208.0), metrics(240.0)),
    )
    .shadow(super::appearance::control_shadow(colors, false))
}

fn destructive_rows(colors: &ChromeColors) -> spaceterm_ui::ListRowPaints {
    use spaceterm_ui::{ListRowPaint, ListRowPaints};
    let elevated = colors.elevated_surface_background;
    let row = |background: Color, foreground: Color, icon: Color, border: Color| {
        let background = background.source_over(elevated);
        let foreground = super::control_theme_catalog::readable_on(foreground, background, 4.5);
        let icon = super::control_theme_catalog::readable_on(icon, background, 4.5);
        ListRowPaint::new(
            gpui_color(background),
            gpui_color(foreground),
            gpui_color(foreground),
            gpui_color(icon),
            gpui_color(foreground),
            gpui_color(border),
        )
    };
    ListRowPaints::new(
        row(elevated, colors.error, colors.error, colors.row_border),
        row(
            colors.row_hover_background,
            colors.error,
            colors.error,
            colors.row_hover_border,
        ),
        row(
            colors.row_selected_background,
            colors.error,
            colors.error,
            colors.row_selected_border,
        ),
        row(
            colors.row_selected_hover_background,
            colors.error,
            colors.error,
            colors.row_selected_hover_border,
        ),
        ListRowPaint::new(
            gpui_color(elevated),
            gpui_color(colors.text_disabled),
            gpui_color(colors.text_disabled),
            gpui_color(colors.icon_disabled),
            gpui_color(colors.text_disabled),
            gpui_color(colors.row_border),
        ),
    )
}

fn metrics(width: f32) -> MenuMetrics {
    MenuMetrics::new(px(width), px(26.0))
        .trigger_height(px(28.0))
        .horizontal_padding(px(6.0))
        .indicator_width(px(16.0))
        .gap(px(6.0))
        .corner_radius(px(8.0))
        .border_width(px(1.0))
        .font_sizes(px(12.0), px(11.0))
        .panel_spacing(px(3.0), px(2.0))
        .decoration_metrics(px(14.0), px(1.0))
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
            let rows = destructive_rows(&colors);

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
                ListRowPaint::new(
                    gpui_color(colors.elevated_surface_background),
                    gpui_color(colors.text_disabled),
                    gpui_color(colors.text_disabled),
                    gpui_color(colors.icon_disabled),
                    gpui_color(colors.text_disabled),
                    gpui_color(colors.row_border),
                )
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
