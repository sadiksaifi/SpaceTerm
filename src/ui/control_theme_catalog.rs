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
