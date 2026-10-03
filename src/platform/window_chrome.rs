//! Host facts for ordinary application windows and their native decoration policy.

use gpui::{App, TitlebarOptions, WindowDecorations, WindowKind, WindowOptions, px};
use std::rc::Rc;

/// Space for the client frame's shadow outside each untiled edge.
pub(crate) const CLIENT_FRAME_INSET: f32 = 24.0;

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
}

impl WindowChrome {
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn native(titlebar: Option<TitlebarOptions>) -> Self {
        Self {
            titlebar: titlebar.map(Rc::new),
            client: false,
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
            options.client_inset = px(CLIENT_FRAME_INSET);
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
                assert_eq!(options.client_inset, px(24.));
            }
        });
    }
}
