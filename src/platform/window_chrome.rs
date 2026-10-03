//! Host facts for ordinary application windows and their native decoration policy.

use gpui::{App, TitlebarOptions, WindowBounds, WindowDecorations, WindowKind, WindowOptions, px};
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
        cx: &App,
    ) -> WindowOptions {
        options.app_id = crate::app::window_application_id();
        if matches!(role, WindowRole::SidebarWindow) {
            options.kind = WindowKind::Normal;
        }
        if self.client {
            options.window_decorations = Some(WindowDecorations::Client);
            // X11 without a compositor uses server decorations and needs no shadow gutter.
            if cx.window_background_support().transparent {
                let extra = px(CLIENT_FRAME_INSET * 2.0);
                // A resizable toplevel's compositor preserves its requested geometry when the
                // client frame is published. Fixed windows already have surface-size constraints,
                // so their initial surface must reserve the shadow before those limits are set.
                if !options.is_resizable
                    && let Some(WindowBounds::Windowed(bounds)) = options.window_bounds.as_mut()
                {
                    bounds.origin.x -= extra / 2.0;
                    bounds.origin.y -= extra / 2.0;
                    bounds.size.width += extra;
                    bounds.size.height += extra;
                }
                if let Some(minimum) = options.window_min_size.as_mut() {
                    minimum.width += extra;
                    minimum.height += extra;
                }
            }
        }
        options
    }
}

impl gpui::Global for WindowChrome {}
