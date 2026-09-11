use spaceterm_ui::ControlThemeCatalog;

use super::{
    button_theme, combo_box_theme, command_palette_theme, menu_theme, modal_theme,
    resize_handle_theme, scrollbar_theme, text_input_theme, tooltip_theme,
};

pub(super) fn catalog(appearance: &super::appearance::ChromeAppearance) -> ControlThemeCatalog {
    let colors = &appearance.colors;
    ControlThemeCatalog::new(
        button_theme::theme(colors),
        scrollbar_theme::theme(colors),
        resize_handle_theme::theme(colors),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::{ChromeColors, Color};

    #[test]
    fn list_themes_should_share_one_background_role() {
        let base = ChromeColors::default();
        let changed = ChromeColors {
            list_item_background: Color::rgb(0x123456),
            ..base.clone()
        };
        assert_ne!(menu_theme::theme(&base), menu_theme::theme(&changed));
        assert_ne!(
            combo_box_theme::theme(&base),
            combo_box_theme::theme(&changed)
        );
        assert_ne!(
            command_palette_theme::theme(&base),
            command_palette_theme::theme(&changed)
        );

        let unrelated = ChromeColors {
            element_hover: Color::rgb(0xff0000),
            element_selected: Color::rgb(0x00ff00),
            ghost_element_selected: Color::rgb(0x0000ff),
            ghost_element_selected_hover: Color::rgb(0xffff00),
            ..changed.clone()
        };
        assert_eq!(menu_theme::theme(&changed), menu_theme::theme(&unrelated));
        assert_eq!(
            combo_box_theme::theme(&changed),
            combo_box_theme::theme(&unrelated)
        );
        assert_eq!(
            command_palette_theme::theme(&changed),
            command_palette_theme::theme(&unrelated)
        );
    }
}
