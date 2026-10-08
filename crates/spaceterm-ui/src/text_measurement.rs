use gpui::{Pixels, Window, px};

/// Reserves a measured text width and the fixed lengths surrounding it.
///
/// Text rounds upward to device pixels. Each padding edge, gap, icon, or other fixed length
/// snaps separately with GPUI's midpoint rule before summation. Pass borders after stroke
/// snapping, which keeps a positive border at least one device pixel wide.
pub fn reserve_measured_width(
    text: Pixels,
    fixed_parts: impl IntoIterator<Item = Pixels>,
    window: &Window,
) -> Pixels {
    let scale = window.scale_factor();
    let fixed_device_pixels: f32 = fixed_parts
        .into_iter()
        .map(|part| (f32::from(window.pixel_snap(part)) * scale).round())
        .sum();
    px(((f32::from(text) * scale).ceil() + fixed_device_pixels) / scale)
}

pub(crate) fn snap_border_width(width: Pixels, window: &Window) -> Pixels {
    if width == px(0.0) {
        width
    } else {
        window
            .pixel_snap(width)
            .max(px(1.0 / window.scale_factor()))
    }
}
