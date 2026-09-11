use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ModalMetrics, ModalPaint, ModalTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ModalTheme {
    ModalTheme::new(
        paint(colors),
        ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
    )
}

fn paint(colors: &ChromeColors) -> ModalPaint {
    ModalPaint::new(
        gpui_color(colors.modal_scrim),
        gpui_color(colors.elevated_surface_background),
        gpui_color(colors.border),
        gpui_color(colors.text),
        gpui_color(colors.text_muted),
        gpui_color(colors.border_variant),
        gpui_color(colors.element_background),
        gpui_color(colors.text_accent),
        gpui_color(colors.info),
        gpui_color(colors.info_background),
        gpui_color(colors.warning),
        gpui_color(colors.warning_background),
        gpui_color(colors.error),
        gpui_color(colors.error_background),
    )
    .suppression_checkbox(
        gpui_color(colors.modal_checkbox_selected),
        gpui_color(colors.modal_checkbox),
        gpui_color(colors.modal_checkbox_focused),
        gpui_color(colors.modal_checkbox_disabled),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::VAGUE_PRO as ACTIVE_THEME;

    #[test]
    fn modal_paint_consumes_the_canonical_scrim_token_directly() {
        let expected = ModalPaint::new(
            gpui_color(ACTIVE_THEME.modal_scrim),
            gpui_color(ACTIVE_THEME.elevated_surface_background),
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.text),
            gpui_color(ACTIVE_THEME.text_muted),
            gpui_color(ACTIVE_THEME.border_variant),
            gpui_color(ACTIVE_THEME.element_background),
            gpui_color(ACTIVE_THEME.text_accent),
            gpui_color(ACTIVE_THEME.info),
            gpui_color(ACTIVE_THEME.info_background),
            gpui_color(ACTIVE_THEME.warning),
            gpui_color(ACTIVE_THEME.warning_background),
            gpui_color(ACTIVE_THEME.error),
            gpui_color(ACTIVE_THEME.error_background),
        )
        .suppression_checkbox(
            gpui_color(ACTIVE_THEME.text_accent),
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.border_focused),
            gpui_color(ACTIVE_THEME.text_disabled),
        );

        assert_eq!(paint(&ChromeColors::default()), expected);
    }
}
