use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ResizeHandleMetrics, ResizeHandlePaint, ResizeHandleTheme};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) const VISIBLE_THICKNESS: f32 = 1.0;

pub(super) fn theme() -> ResizeHandleTheme {
    ResizeHandleTheme::new(
        ResizeHandlePaint::new(
            gpui_color(ACTIVE_THEME.border),
            gpui_color(ACTIVE_THEME.panel_indent_guide_hover),
            gpui_color(ACTIVE_THEME.panel_indent_guide_active),
            gpui_color(ACTIVE_THEME.panel_focused_border),
            gpui_color(ACTIVE_THEME.border_disabled),
        ),
        ResizeHandleMetrics::new(px(VISIBLE_THICKNESS), px(8.0)),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
