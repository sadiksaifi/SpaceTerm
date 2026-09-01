use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ModalMetrics, ModalPaint, ModalTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> ModalTheme {
    ModalTheme::new(paint(), ModalMetrics::new(px(400.0), px(480.0), px(640.0)))
}

fn paint() -> ModalPaint {
    ModalPaint::new(
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
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );

        assert_eq!(paint(), expected);
    }
}
