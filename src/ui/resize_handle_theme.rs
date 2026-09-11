use gpui::{Pixels, Rgba, px, rgba};
use spaceterm_ui::{ResizeHandleMetrics, ResizeHandlePaint, ResizeHandleTarget, ResizeHandleTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) const VISIBLE_THICKNESS: f32 = 1.0;

pub(super) fn theme(colors: &ChromeColors) -> ResizeHandleTheme {
    ResizeHandleTheme::new(
        ResizeHandlePaint::new(
            gpui_color(colors.resize_idle),
            gpui_color(colors.resize_focused),
            gpui_color(colors.resize_hovered),
            gpui_color(colors.resize_dragged),
            gpui_color(colors.resize_disabled),
        ),
        ResizeHandleMetrics::new(px(VISIBLE_THICKNESS), px(8.0)),
    )
}

pub(super) fn spacious_target_half_thickness(cx: &gpui::App) -> Pixels {
    let thickness = cx
        .global::<ResizeHandleTheme>()
        .pointer_target_thickness(ResizeHandleTarget::Spacious);
    px(f32::from(thickness) / 2.0)
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
