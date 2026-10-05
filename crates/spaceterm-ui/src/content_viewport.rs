//! Visible application content, excluding client-frame shadow gutters.

use gpui::{Bounds, Decorations, Pixels, Window, point, px, size};

/// Bounds within which floating application surfaces may be placed.
pub fn content_viewport(window: &Window) -> Bounds<Pixels> {
    let viewport = window.viewport_size();
    let Decorations::Client { tiling } = window.window_decorations() else {
        return Bounds::new(point(px(0.0), px(0.0)), viewport);
    };
    let inset = window.client_inset().unwrap_or(px(0.0));
    let full = window.is_maximized() || window.is_fullscreen();
    let left = if full || tiling.left { px(0.0) } else { inset };
    let right = if full || tiling.right { px(0.0) } else { inset };
    let top = if full || tiling.top { px(0.0) } else { inset };
    let bottom = if full || tiling.bottom {
        px(0.0)
    } else {
        inset
    };
    Bounds::new(
        point(left, top),
        size(
            (viewport.width - left - right).max(px(0.0)),
            (viewport.height - top - bottom).max(px(0.0)),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn content_bounds_exclude_only_untiled_client_frame_edges(cx: &mut gpui::TestAppContext) {
        let cx = cx.add_empty_window();
        cx.simulate_decorations(Decorations::Client {
            tiling: gpui::Tiling {
                top: true,
                right: true,
                ..Default::default()
            },
        });
        cx.update(|window, _| {
            window.set_client_inset(px(24.0));
            assert_eq!(
                content_viewport(window),
                Bounds::new(
                    point(px(24.0), px(0.0)),
                    size(
                        window.viewport_size().width - px(24.0),
                        window.viewport_size().height - px(24.0)
                    )
                )
            );
        });
        cx.simulate_decorations(Decorations::Server);
        cx.update(|window, _| assert_eq!(content_viewport(window).size, window.viewport_size()));
    }
}
