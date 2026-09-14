use spaceterm_ui::ControlThemeCatalog;

use super::{
    button_theme, combo_box_theme, command_palette_theme, menu_theme, modal_theme,
    resize_handle_theme, scrollbar_theme, segmented_control_theme, text_input_theme, toggle_theme,
    tooltip_theme,
};

pub(super) fn catalog(appearance: &super::appearance::ChromeAppearance) -> ControlThemeCatalog {
    let colors = &appearance.colors;
    ControlThemeCatalog::new(
        button_theme::theme(colors),
        toggle_theme::theme(colors),
        scrollbar_theme::theme(colors),
        resize_handle_theme::theme(colors),
        segmented_control_theme::theme(colors),
        menu_theme::theme(colors),
        command_palette_theme::theme(colors),
        combo_box_theme::theme(colors),
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

pub(super) fn overlay_list_rows(
    colors: &crate::appearance::ChromeColors,
) -> spaceterm_ui::ListRowPaints {
    use spaceterm_ui::{ListRowPaint, ListRowPaints};
    let elevated = colors.elevated_surface_background;
    let row = |background: crate::appearance::Color,
               foreground: crate::appearance::Color,
               secondary: crate::appearance::Color,
               icon: crate::appearance::Color,
               matched: crate::appearance::Color,
               border: crate::appearance::Color| {
        let background = background.source_over(elevated);
        ListRowPaint::new(
            gpui_color(background),
            gpui_color(readable_on(foreground, background, 4.5)),
            gpui_color(readable_on(secondary, background, 4.5)),
            gpui_color(readable_on(icon, background, 4.5)),
            gpui_color(readable_on(matched, background, 4.5)),
            gpui_color(border),
        )
    };
    ListRowPaints::new(
        row(
            elevated,
            colors.row_foreground,
            colors.row_secondary,
            colors.row_icon,
            colors.row_match,
            colors.row_border,
        ),
        row(
            colors.row_hover_background,
            colors.row_hover_foreground,
            colors.row_hover_secondary,
            colors.row_hover_icon,
            colors.row_hover_match,
            colors.row_hover_border,
        ),
        row(
            colors.row_selected_background,
            colors.row_selected_foreground,
            colors.row_selected_secondary,
            colors.row_selected_icon,
            colors.row_selected_match,
            colors.row_selected_border,
        ),
        row(
            colors.row_selected_hover_background,
            colors.row_selected_hover_foreground,
            colors.row_selected_hover_secondary,
            colors.row_selected_hover_icon,
            colors.row_selected_hover_match,
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
        let rows = overlay_list_rows(&colors);
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
