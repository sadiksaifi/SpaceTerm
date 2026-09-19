use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ModalMetrics, ModalPaint, ModalTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> ModalTheme {
    ModalTheme::new(
        paint(colors),
        ModalMetrics::new(px(360.0), px(480.0), px(640.0)),
    )
}

/// The modal's own meaning: two registers of text and three semantic intents.
///
/// The scrim, the surface material, its edge, and its internal rules are resolved once for every
/// floating surface in the window and are not authored again here.
fn paint(colors: &ChromeColors) -> ModalPaint {
    ModalPaint::new(
        gpui_color(colors.text),
        gpui_color(colors.text_muted),
        gpui_color(colors.info),
        gpui_color(colors.info_background),
        gpui_color(colors.warning),
        gpui_color(colors.warning_background),
        gpui_color(colors.error),
        gpui_color(colors.error_background),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn floating_surface_consumes_the_canonical_scrim_token_directly() {
        let colors = ChromeColors {
            modal_scrim: Color::rgba(0x12345678),
            ..ChromeColors::default()
        };
        let appearance = crate::ui::appearance::ChromeAppearance {
            colors,
            ..crate::ui::appearance::ChromeAppearance::default()
        };
        assert_eq!(appearance.floating_surfaces().scrim(), rgba(0x12345678));
    }
}
