use crate::ui::appearance::gpui_color;
use gpui::px;
use spaceterm_ui::{ModalMetrics, ModalPaint, ModalTheme};

use crate::appearance::ChromeColors;

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
        gpui_color(colors.warning),
        gpui_color(colors.error),
    )
}
