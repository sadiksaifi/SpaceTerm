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

pub(super) fn list_rows(colors: &crate::appearance::ChromeColors) -> spaceterm_ui::ListRowPaints {
    use gpui::rgba;
    use spaceterm_ui::{ListRowPaint, ListRowPaints};
    ListRowPaints::new(
        ListRowPaint::new(
            rgba(colors.row_background.rgba_hex()),
            rgba(colors.row_foreground.rgba_hex()),
            rgba(colors.row_secondary.rgba_hex()),
            rgba(colors.row_icon.rgba_hex()),
            rgba(colors.row_match.rgba_hex()),
            rgba(colors.row_border.rgba_hex()),
        ),
        ListRowPaint::new(
            rgba(colors.row_hover_background.rgba_hex()),
            rgba(colors.row_hover_foreground.rgba_hex()),
            rgba(colors.row_hover_secondary.rgba_hex()),
            rgba(colors.row_hover_icon.rgba_hex()),
            rgba(colors.row_hover_match.rgba_hex()),
            rgba(colors.row_hover_border.rgba_hex()),
        ),
        ListRowPaint::new(
            rgba(colors.row_selected_background.rgba_hex()),
            rgba(colors.row_selected_foreground.rgba_hex()),
            rgba(colors.row_selected_secondary.rgba_hex()),
            rgba(colors.row_selected_icon.rgba_hex()),
            rgba(colors.row_selected_match.rgba_hex()),
            rgba(colors.row_selected_border.rgba_hex()),
        ),
        ListRowPaint::new(
            rgba(colors.row_selected_hover_background.rgba_hex()),
            rgba(colors.row_selected_hover_foreground.rgba_hex()),
            rgba(colors.row_selected_hover_secondary.rgba_hex()),
            rgba(colors.row_selected_hover_icon.rgba_hex()),
            rgba(colors.row_selected_hover_match.rgba_hex()),
            rgba(colors.row_selected_hover_border.rgba_hex()),
        ),
        ListRowPaint::new(
            rgba(colors.row_background.rgba_hex()),
            rgba(colors.text_disabled.rgba_hex()),
            rgba(colors.text_disabled.rgba_hex()),
            rgba(colors.icon_disabled.rgba_hex()),
            rgba(colors.text_disabled.rgba_hex()),
            rgba(colors.row_border.rgba_hex()),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{ChromeColors, Color};

    #[test]
    fn list_rows_preserve_every_authored_channel_for_combined_states() {
        use gpui::rgba;
        use spaceterm_ui::ListRowPaint;
        let mut colors = ChromeColors::default();
        colors.row_background = Color::rgba(0x10203040);
        colors.row_foreground = Color::rgba(0x14233142);
        colors.row_secondary = Color::rgba(0x18263244);
        colors.row_icon = Color::rgba(0x1c293346);
        colors.row_match = Color::rgba(0x202c3448);
        colors.row_border = Color::rgba(0x242f354a);
        colors.row_hover_background = Color::rgba(0x2832364c);
        colors.row_hover_foreground = Color::rgba(0x2c35374e);
        colors.row_hover_secondary = Color::rgba(0x30383850);
        colors.row_hover_icon = Color::rgba(0x343b3952);
        colors.row_hover_match = Color::rgba(0x383e3a54);
        colors.row_hover_border = Color::rgba(0x3c413b56);
        colors.row_selected_background = Color::rgba(0x40443c58);
        colors.row_selected_foreground = Color::rgba(0x44473d5a);
        colors.row_selected_secondary = Color::rgba(0x484a3e5c);
        colors.row_selected_icon = Color::rgba(0x4c4d3f5e);
        colors.row_selected_match = Color::rgba(0x50504060);
        colors.row_selected_border = Color::rgba(0x54534162);
        colors.row_selected_hover_background = Color::rgba(0x58564264);
        colors.row_selected_hover_foreground = Color::rgba(0x5c594366);
        colors.row_selected_hover_secondary = Color::rgba(0x605c4468);
        colors.row_selected_hover_icon = Color::rgba(0x645f456a);
        colors.row_selected_hover_match = Color::rgba(0x6862466c);
        colors.row_selected_hover_border = Color::rgba(0x6c65476e);
        let rows = list_rows(&colors);
        assert_eq!(
            rows.resolve(true, false, false),
            ListRowPaint::new(
                rgba(colors.row_background.rgba_hex()),
                rgba(colors.row_foreground.rgba_hex()),
                rgba(colors.row_secondary.rgba_hex()),
                rgba(colors.row_icon.rgba_hex()),
                rgba(colors.row_match.rgba_hex()),
                rgba(colors.row_border.rgba_hex()),
            )
        );
        assert_eq!(
            rows.resolve(true, false, true),
            ListRowPaint::new(
                rgba(colors.row_hover_background.rgba_hex()),
                rgba(colors.row_hover_foreground.rgba_hex()),
                rgba(colors.row_hover_secondary.rgba_hex()),
                rgba(colors.row_hover_icon.rgba_hex()),
                rgba(colors.row_hover_match.rgba_hex()),
                rgba(colors.row_hover_border.rgba_hex()),
            )
        );
        assert_eq!(
            rows.resolve(true, true, false),
            ListRowPaint::new(
                rgba(colors.row_selected_background.rgba_hex()),
                rgba(colors.row_selected_foreground.rgba_hex()),
                rgba(colors.row_selected_secondary.rgba_hex()),
                rgba(colors.row_selected_icon.rgba_hex()),
                rgba(colors.row_selected_match.rgba_hex()),
                rgba(colors.row_selected_border.rgba_hex()),
            )
        );
        assert_eq!(
            rows.resolve(true, true, true),
            ListRowPaint::new(
                rgba(colors.row_selected_hover_background.rgba_hex()),
                rgba(colors.row_selected_hover_foreground.rgba_hex()),
                rgba(colors.row_selected_hover_secondary.rgba_hex()),
                rgba(colors.row_selected_hover_icon.rgba_hex()),
                rgba(colors.row_selected_hover_match.rgba_hex()),
                rgba(colors.row_selected_hover_border.rgba_hex()),
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
}
