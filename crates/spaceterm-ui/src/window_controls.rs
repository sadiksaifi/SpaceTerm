//! Client-drawn window controls. The owner retains authority over closing its window.

use std::rc::Rc;

use gpui::{App, Decorations, Window, WindowButton, WindowButtonLayout, div, prelude::*, px};

use crate::{ButtonVariant, CustomIconName, Icon, IconButton, Tooltip};

/// The owning surface's close operation, including its confirmation and cleanup policy.
pub type WindowCloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Desktop-selected controls, presented together on the trailing edge of application chrome.
#[derive(IntoElement)]
pub struct ClientWindowControls {
    close: WindowCloseHandler,
}

impl ClientWindowControls {
    /// Construct controls whose close request is handled by the window owner.
    pub fn new(close: WindowCloseHandler) -> Self {
        Self { close }
    }
}

fn trailing_buttons(
    layout: Option<WindowButtonLayout>,
    capabilities: gpui::WindowControls,
    resizable: bool,
    minimizable: bool,
) -> Vec<WindowButton> {
    let layout = layout.unwrap_or(WindowButtonLayout {
        left: [None; 3],
        right: [
            Some(WindowButton::Minimize),
            Some(WindowButton::Maximize),
            Some(WindowButton::Close),
        ],
    });
    let mut buttons = Vec::with_capacity(3);
    for button in layout.left.into_iter().chain(layout.right).flatten() {
        let allowed = match button {
            WindowButton::Minimize => capabilities.minimize && minimizable,
            WindowButton::Maximize => capabilities.maximize && resizable,
            WindowButton::Close => true,
        };
        if allowed && !buttons.contains(&button) {
            buttons.push(button);
        }
    }
    buttons
}

impl RenderOnce for ClientWindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        window.use_keyed_state("client-window-controls-observer", cx, |window, _| {
            window.observe_button_layout_changed(|window, _| window.refresh())
        });
        let mut controls = div()
            .id("client-window-controls")
            .flex_none()
            .flex()
            .items_center()
            .gap(px(6.0));
        if !matches!(window.window_decorations(), Decorations::Client { .. })
            || window.is_fullscreen()
        {
            return controls;
        }
        for button in trailing_buttons(
            cx.button_layout(),
            window.window_controls(),
            window.is_resizable(),
            window.is_minimizable(),
        ) {
            let (id, label, icon) = match button {
                WindowButton::Minimize => (
                    "window-minimize",
                    "Minimize",
                    CustomIconName::WindowMinimize,
                ),
                WindowButton::Maximize if window.is_maximized() => {
                    ("window-maximize", "Restore", CustomIconName::WindowRestore)
                }
                WindowButton::Maximize => (
                    "window-maximize",
                    "Maximize",
                    CustomIconName::WindowMaximize,
                ),
                WindowButton::Close => ("window-close", "Close", CustomIconName::WindowClose),
            };
            let close = self.close.clone();
            let activate: WindowCloseHandler = Rc::new(move |window, cx| match button {
                WindowButton::Minimize => window.minimize_window(),
                WindowButton::Maximize => window.zoom_window(),
                WindowButton::Close => close(window, cx),
            });
            let accessible_activate = activate.clone();
            controls = controls.child(
                div()
                    .id(format!("{id}-accessible"))
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, cx| {
                        accessible_activate(window, cx);
                    })
                    .block_mouse_except_scroll()
                    .child(
                        IconButton::new(id, label, move |color| {
                            Icon::custom(icon, px(12.0), color).into_any_element()
                        })
                        .variant(ButtonVariant::Ghost)
                        .target_size(px(24.0))
                        .corner_radius(px(12.0))
                        .debug_selector(id)
                        .tooltip(Tooltip::new(format!("{id}-tooltip"), label))
                        .on_activate(move |_, window, cx| activate(window, cx)),
                    ),
            );
        }
        controls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_order_and_capabilities_control_the_trailing_buttons() {
        use WindowButton::*;
        let layout = WindowButtonLayout {
            left: [Some(Close), Some(Minimize), None],
            right: [Some(Maximize), Some(Close), None],
        };
        assert_eq!(
            trailing_buttons(Some(layout), gpui::WindowControls::default(), true, true),
            vec![Close, Minimize, Maximize]
        );
        assert_eq!(
            trailing_buttons(Some(layout), gpui::WindowControls::default(), false, false),
            vec![Close]
        );
        assert_eq!(
            trailing_buttons(
                None,
                gpui::WindowControls {
                    maximize: false,
                    minimize: false,
                    ..Default::default()
                },
                true,
                true
            ),
            vec![Close]
        );
        assert_eq!(
            trailing_buttons(
                Some(WindowButtonLayout {
                    left: [None; 3],
                    right: [None; 3]
                }),
                gpui::WindowControls::default(),
                true,
                true
            ),
            vec![]
        );
    }
}
