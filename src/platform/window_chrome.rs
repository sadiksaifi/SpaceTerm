//! Host facts for ordinary application windows and their native decoration policy.

use gpui::{
    App, BoxShadow, TitlebarOptions, WindowDecorations, WindowKind, WindowOptions, point, px, rgba,
};
use std::rc::Rc;

/// Blur radius of the client frame's drop shadow. GPUI paints it as a Gaussian whose sigma is this
/// radius and whose visible extent is three sigmas.
pub(crate) const CLIENT_FRAME_SHADOW_BLUR: f32 = 16.0;
/// Downward offset of the client frame's drop shadow.
pub(crate) const CLIENT_FRAME_SHADOW_OFFSET_Y: f32 = 6.0;
/// Space for the client frame's shadow outside each untiled edge: the shadow's full extent, so the
/// surface edge never cuts it off.
pub(crate) const CLIENT_FRAME_INSET: f32 =
    3.0 * CLIENT_FRAME_SHADOW_BLUR + 16.0 + CLIENT_FRAME_SHADOW_OFFSET_Y;

pub(crate) fn frame_shadows(
    style: spaceterm_ui::DesktopWindowStyle,
    active: bool,
) -> Vec<BoxShadow> {
    use spaceterm_ui::DesktopWindowStyle::*;
    // GTK's CSS blur radius is twice Gaussian sigma. Keep the widest inactive layer transparent
    // so activation never changes the reserved surface size.
    let layers: &[(f32, f32, f32, f32)] = match (style, active) {
        (Adwaita, true) => &[
            (4.0, 2.0, 2.0, 0.13),
            (10.0, 10.0, 3.0, 0.09),
            (
                CLIENT_FRAME_SHADOW_BLUR,
                16.0,
                CLIENT_FRAME_SHADOW_OFFSET_Y,
                0.04,
            ),
        ],
        (Adwaita, false) => &[
            (1.5, 3.0, 1.0, 0.09),
            (7.0, 5.0, 2.0, 0.05),
            (14.0, 12.0, 4.0, 0.03),
            (
                CLIENT_FRAME_SHADOW_BLUR,
                16.0,
                CLIENT_FRAME_SHADOW_OFFSET_Y,
                0.0,
            ),
        ],
        (Breeze, true) => &[(24.0, 0.0, 12.0, 0.8), (12.0, 0.0, 6.0, 0.2)],
        (Breeze, false) => &[(24.0, 0.0, 12.0, 0.4), (12.0, 0.0, 6.0, 0.1)],
    };
    layers
        .iter()
        .map(|&(blur, spread, offset, alpha)| {
            let mut color = rgba(0x000000ff);
            color.a = alpha;
            BoxShadow {
                color: color.into(),
                offset: point(px(0.0), px(offset)),
                blur_radius: px(blur),
                spread_radius: px(spread),
                inset: false,
            }
        })
        .collect()
}

pub(crate) fn frame_inset(style: spaceterm_ui::DesktopWindowStyle) -> f32 {
    let inset = [true, false]
        .into_iter()
        .flat_map(|active| frame_shadows(style, active))
        .map(|shadow| {
            3.0 * f32::from(shadow.blur_radius)
                + f32::from(shadow.spread_radius)
                + f32::from(shadow.offset.y).abs()
        })
        .fold(0.0, f32::max)
        .ceil();
    if style == spaceterm_ui::DesktopWindowStyle::Adwaita {
        debug_assert_eq!(inset, CLIENT_FRAME_INSET);
    }
    inset
}

#[derive(Clone, Copy)]
pub(crate) enum WindowRole {
    Workspace,
    SidebarWindow,
    Launch,
}

#[derive(Clone)]
pub(crate) struct WindowChrome {
    pub(crate) titlebar: Option<Rc<TitlebarOptions>>,
    client: bool,
    controls: Option<spaceterm_ui::DesktopWindowControls>,
}

impl WindowChrome {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn native(titlebar: Option<TitlebarOptions>) -> Self {
        Self {
            titlebar: titlebar.map(Rc::new),
            client: false,
            controls: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn client() -> Self {
        Self {
            titlebar: Some(Rc::new(TitlebarOptions {
                title: Some(
                    crate::application_identity::ApplicationIdentity::current()
                        .display_name()
                        .into(),
                ),
                appears_transparent: true,
                traffic_light_position: None,
            })),
            client: true,
            controls: Some(Default::default()),
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn with_controls(mut self, controls: spaceterm_ui::DesktopWindowControls) -> Self {
        self.controls = Some(controls);
        self
    }

    pub(crate) fn install_controls(&self, cx: &mut App) {
        if let Some(controls) = &self.controls {
            cx.set_global(controls.clone());
        }
    }

    /// Apply the host's frame policy to content dimensions and role-specific behavior.
    pub(crate) fn options(
        &self,
        role: WindowRole,
        mut options: WindowOptions,
        _cx: &App,
    ) -> WindowOptions {
        options.app_id = crate::app::window_application_id();
        if matches!(role, WindowRole::SidebarWindow) {
            options.kind = WindowKind::Normal;
        }
        if self.client {
            options.window_decorations = Some(WindowDecorations::Client);
            // Native backends reserve this gutter only for the actual client decoration mode.
            // Bounds and minimum size describe visible content on every backend.
            options.client_inset =
                px(frame_inset(self.controls.as_ref().map_or(
                    spaceterm_ui::DesktopWindowStyle::default(),
                    |controls| controls.style,
                )));
        }
        options
    }
}

impl gpui::Global for WindowChrome {}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use gpui::{Bounds, WindowBounds, point, size};

    #[gpui::test]
    fn linux_workspace_options_describe_visible_content_for_native_frame_conversion(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            let content = Bounds::new(point(px(100.), px(200.)), size(px(900.), px(580.)));
            for resizable in [true, false] {
                let options = WindowChrome::client().options(
                    WindowRole::Workspace,
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(content)),
                        window_min_size: Some(size(px(480.), px(260.))),
                        is_resizable: resizable,
                        ..Default::default()
                    },
                    cx,
                );
                assert_eq!(options.window_bounds, Some(WindowBounds::Windowed(content)));
                assert_eq!(options.window_min_size, Some(size(px(480.), px(260.))));
                assert_eq!(options.window_decorations, Some(WindowDecorations::Client));
                assert_eq!(options.client_inset, px(CLIENT_FRAME_INSET));
            }
        });
    }
}

#[cfg(test)]
mod native_frame_tests {
    use super::*;
    #[test]
    fn native_frame_inset_contains_every_active_and_inactive_shadow() {
        for style in [
            spaceterm_ui::DesktopWindowStyle::Adwaita,
            spaceterm_ui::DesktopWindowStyle::Breeze,
        ] {
            let inset = frame_inset(style);
            for active in [true, false] {
                for shadow in frame_shadows(style, active) {
                    let extent =
                        3.0 * f32::from(shadow.blur_radius) + f32::from(shadow.spread_radius);
                    assert!(extent + f32::from(shadow.offset.y).abs() <= inset);
                    assert!(extent + f32::from(shadow.offset.x).abs() <= inset);
                }
            }
        }
        assert_eq!(
            frame_inset(spaceterm_ui::DesktopWindowStyle::Adwaita),
            CLIENT_FRAME_INSET
        );
    }
}
