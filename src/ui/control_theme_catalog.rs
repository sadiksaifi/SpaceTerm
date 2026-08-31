use spaceterm_ui::ControlThemeCatalog;

use super::{
    button_theme, command_palette_theme, menu_theme, modal_theme, resize_handle_theme,
    scrollbar_theme, text_input_theme, tooltip_theme,
};

pub(super) fn catalog() -> ControlThemeCatalog {
    ControlThemeCatalog::new(
        button_theme::theme(),
        scrollbar_theme::theme(),
        resize_handle_theme::theme(),
        menu_theme::theme(),
        command_palette_theme::theme(),
        text_input_theme::theme(),
        tooltip_theme::theme(),
        modal_theme::theme(),
    )
}
