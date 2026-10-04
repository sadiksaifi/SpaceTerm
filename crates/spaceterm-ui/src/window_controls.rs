//! Desktop-owned presentation and client-drawn window controls.

use crate::{ButtonPaint, ButtonVariantStyle, IconButton};
use gpui::{
    App, Decorations, Rgba, Window, WindowButton, WindowButtonLayout, div, prelude::*, px, rgba,
    svg,
};
use std::{rc::Rc, sync::Arc};

/// The owning surface's close operation, including its confirmation and cleanup policy.
pub type WindowCloseHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The desktop's native decoration family. Unknown desktops use Adwaita.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DesktopWindowStyle {
    /// GNOME's libadwaita window controls and frame.
    #[default]
    Adwaita,
    /// KDE's Breeze window controls and frame.
    Breeze,
}

impl DesktopWindowStyle {
    /// Selects a desktop from XDG's colon-separated desktop names and a settings theme hint.
    pub fn select(desktops: &str, theme: &str) -> Self {
        if desktops
            .split(':')
            .any(|name| name.eq_ignore_ascii_case("KDE") || name.eq_ignore_ascii_case("PLASMA"))
            || (desktops.is_empty() && theme.to_ascii_lowercase().starts_with("breeze"))
        {
            Self::Breeze
        } else {
            Self::Adwaita
        }
    }

    /// The native outer corner radius, in logical pixels.
    pub fn corner_radius(self) -> f32 {
        match self {
            Self::Adwaita => 15.0,
            Self::Breeze => 5.0,
        }
    }

    /// The native control geometry.
    pub fn control_metrics(self) -> DesktopControlMetrics {
        match self {
            Self::Adwaita => DesktopControlMetrics {
                target: 34.0,
                diameter: 24.0,
                gap: 3.0,
                edge_margin: 7.0,
            },
            Self::Breeze => DesktopControlMetrics {
                target: 20.0,
                diameter: 18.0,
                gap: 4.0,
                edge_margin: 4.0,
            },
        }
    }

    /// The running desktop's style, or the Adwaita fallback before the host publishes one.
    pub fn current(cx: &App) -> Self {
        cx.try_global::<DesktopWindowControls>()
            .map_or(Self::default(), |facts| facts.style)
    }
}

/// Native window control geometry, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DesktopControlMetrics {
    /// The pointer target of each control.
    pub target: f32,
    /// The painted control's diameter, centered in its target.
    pub diameter: f32,
    /// The space between adjacent targets.
    pub gap: f32,
    /// The space between the outermost target and the window's titlebar edge.
    pub edge_margin: f32,
}

/// Host-resolved symbolic icons. No filesystem or desktop discovery occurs in portable rendering.
#[derive(Clone, Default)]
pub struct DesktopWindowControls {
    /// The native desktop decoration family.
    pub style: DesktopWindowStyle,
    /// Close, minimize, maximize and restore SVGs, resolved through the host icon theme.
    pub icons: [Option<Arc<[u8]>>; 4],
}
impl gpui::Global for DesktopWindowControls {}

/// Which side of the window owns this control group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowControlSide {
    /// The leading group, before the sidebar toggle.
    Left,
    /// The trailing group.
    #[default]
    Right,
}

/// Desktop-selected controls on one side of the application chrome.
#[derive(IntoElement)]
pub struct ClientWindowControls {
    close: WindowCloseHandler,
    side: WindowControlSide,
    surface: Option<Rgba>,
}
impl ClientWindowControls {
    /// Construct trailing controls whose close request is handled by the window owner.
    pub fn new(close: WindowCloseHandler) -> Self {
        Self {
            close,
            side: WindowControlSide::Right,
            surface: None,
        }
    }
    /// Select the desktop layout's side without moving controls across the window.
    pub fn side(mut self, side: WindowControlSide) -> Self {
        self.side = side;
        self
    }
    /// Select the native light/dark variant from the actual surrounding chrome tone.
    pub fn surface_color(mut self, surface: Rgba) -> Self {
        self.surface = Some(surface);
        self
    }
    /// Space occupied by a desktop-selected group, including its outer margin.
    pub fn width(side: WindowControlSide, window: &Window, cx: &App) -> gpui::Pixels {
        if window.is_fullscreen()
            || !matches!(window.window_decorations(), Decorations::Client { .. })
        {
            return px(0.0);
        }
        let count = buttons(
            side,
            cx.button_layout(),
            window.window_controls(),
            window.is_resizable(),
            window.is_minimizable(),
        )
        .len();
        if count == 0 {
            return px(0.0);
        }
        let metrics = DesktopWindowStyle::current(cx).control_metrics();
        px(count as f32 * metrics.target + (count - 1) as f32 * metrics.gap + metrics.edge_margin)
    }
}

fn buttons(
    side: WindowControlSide,
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
    let mut result = Vec::with_capacity(3);
    for button in match side {
        WindowControlSide::Left => layout.left,
        WindowControlSide::Right => layout.right,
    }
    .into_iter()
    .flatten()
    {
        let allowed = match button {
            WindowButton::Minimize => capabilities.minimize && minimizable,
            WindowButton::Maximize => capabilities.maximize && resizable,
            WindowButton::Close => true,
        };
        if allowed && !result.contains(&button) {
            result.push(button);
        }
    }
    result
}

fn glyph(button: WindowButton, maximized: bool) -> usize {
    match button {
        WindowButton::Close => 0,
        WindowButton::Minimize => 1,
        WindowButton::Maximize if maximized => 3,
        WindowButton::Maximize => 2,
    }
}

/// Native controls choose a style variant by the luminance of their own chrome surface.
pub fn window_controls_dark(surface: Rgba) -> bool {
    0.2126 * surface.r + 0.7152 * surface.g + 0.0722 * surface.b < 0.5
}

fn paints(
    style: DesktopWindowStyle,
    dark: bool,
    active: bool,
    close: bool,
) -> (ButtonVariantStyle, Rgba) {
    let opacity = if active { 1.0 } else { 0.5 };
    let mut foreground = match (style, dark, active) {
        (DesktopWindowStyle::Breeze, _, false) => rgba(0x7f8c8dff),
        (DesktopWindowStyle::Breeze, true, true) => rgba(0xeff0f1ff),
        (DesktopWindowStyle::Breeze, false, true) => rgba(0x232629ff),
        (DesktopWindowStyle::Adwaita, true, _) => rgba(0xffffffff),
        (DesktopWindowStyle::Adwaita, false, _) => rgba(0x000000cc),
    };
    if style == DesktopWindowStyle::Adwaita {
        foreground.a *= opacity;
    }
    let titlebar = rgba(if dark { 0x31363bff } else { 0xeff0f1ff });
    let background = |alpha: f32| {
        let mut color = if dark {
            rgba(0xffffffff)
        } else {
            rgba(0x000000ff)
        };
        color.a = alpha * opacity;
        color
    };
    let transparent = rgba(0x00000000);
    let (normal, hover, pressed) = match style {
        DesktopWindowStyle::Adwaita => (background(0.1), background(0.15), background(0.3)),
        DesktopWindowStyle::Breeze => (
            transparent,
            if close {
                rgba(if active { 0xff657cff } else { 0xda4453ff })
            } else {
                foreground
            },
            if close {
                rgba(0x6d222aff)
            } else {
                Rgba {
                    r: titlebar.r * 0.7 + foreground.r * 0.3,
                    g: titlebar.g * 0.7 + foreground.g * 0.3,
                    b: titlebar.b * 0.7 + foreground.b * 0.3,
                    a: 1.0,
                }
            },
        ),
    };
    let paint = |background, foreground| ButtonPaint::new(background, foreground, transparent);
    let hover_foreground = if style == DesktopWindowStyle::Breeze {
        titlebar
    } else {
        foreground
    };
    (
        ButtonVariantStyle::new(
            paint(normal, foreground),
            paint(hover, hover_foreground),
            paint(pressed, hover_foreground),
            paint(normal, foreground),
        ),
        rgba(match (style, dark) {
            (DesktopWindowStyle::Breeze, _) => 0x3daee980,
            (DesktopWindowStyle::Adwaita, true) => 0x78aeed80,
            (DesktopWindowStyle::Adwaita, false) => 0x3584e480,
        }),
    )
}

// These fallback vectors use the native symbolic geometry rather than the app icon family.
const ADWAITA: [&[u8]; 4] = [
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M4 4L12 12M12 4L4 12" stroke="black" stroke-width="2"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M4 11H12" stroke="black" stroke-width="2"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M5 5H11V11H5Z" fill="none" stroke="black" stroke-width="2"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><path d="M6 6H10V10H6Z" fill="none" stroke="black" stroke-width="2"/></svg>"#,
];
const BREEZE: [&[u8]; 4] = [
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><path d="M6 6L14 14M14 6L6 14" stroke="black" stroke-width="1.01" stroke-linecap="round" fill="none"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><path d="M5 8L10 13L15 8" stroke="black" stroke-width="1.01" stroke-linecap="round" fill="none"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><path d="M5 12L10 7L15 12" stroke="black" stroke-width="1.01" stroke-linecap="round" fill="none"/></svg>"#,
    br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20"><path d="M5 10L10 5L15 10L10 15Z" stroke="black" stroke-width="1.01" stroke-linejoin="round" fill="none"/></svg>"#,
];

impl RenderOnce for ClientWindowControls {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        window.use_keyed_state("client-window-controls-observer", cx, |window, _| {
            window.observe_button_layout_changed(|window, _| window.refresh())
        });
        let facts = cx
            .try_global::<DesktopWindowControls>()
            .cloned()
            .unwrap_or_default();
        let DesktopControlMetrics {
            target,
            diameter,
            gap,
            ..
        } = facts.style.control_metrics();
        let mut controls = div()
            .id(match self.side {
                WindowControlSide::Left => "client-window-controls-left",
                WindowControlSide::Right => "client-window-controls",
            })
            .flex_none()
            .flex()
            .items_center()
            .gap(px(gap));
        if !matches!(window.window_decorations(), Decorations::Client { .. })
            || window.is_fullscreen()
        {
            return crate::ModalLayer::window_chrome(controls);
        }
        let dark = self.surface.map_or(
            matches!(
                window.appearance(),
                gpui::WindowAppearance::Dark | gpui::WindowAppearance::VibrantDark
            ),
            window_controls_dark,
        );
        let active = window.is_window_active();
        for button in buttons(
            self.side,
            cx.button_layout(),
            window.window_controls(),
            window.is_resizable(),
            window.is_minimizable(),
        ) {
            let index = glyph(button, window.is_maximized());
            let (id, label) = match button {
                WindowButton::Minimize => ("window-minimize", "Minimize"),
                WindowButton::Maximize if window.is_maximized() => ("window-maximize", "Restore"),
                WindowButton::Maximize => ("window-maximize", "Maximize"),
                WindowButton::Close => ("window-close", "Close"),
            };
            let close = self.close.clone();
            let activate: WindowCloseHandler = Rc::new(move |window, cx| match button {
                WindowButton::Minimize => window.minimize_window(),
                WindowButton::Maximize => window.zoom_window(),
                WindowButton::Close => close(window, cx),
            });
            let accessible_activate = activate.clone();
            let icon_size =
                if facts.icons[index].is_none() && facts.style == DesktopWindowStyle::Breeze {
                    20.0
                } else {
                    16.0
                };
            let data = facts.icons[index].clone().unwrap_or_else(|| {
                Arc::from(match facts.style {
                    DesktopWindowStyle::Adwaita => ADWAITA[index],
                    DesktopWindowStyle::Breeze => BREEZE[index],
                })
            });
            let (style, focus) = paints(facts.style, dark, active, button == WindowButton::Close);
            controls = controls.child(
                div()
                    .id(format!("{id}-accessible"))
                    .role(gpui::Role::Button)
                    .aria_label(label)
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, cx| {
                        accessible_activate(window, cx)
                    })
                    .block_mouse_except_scroll()
                    .child(
                        IconButton::new(id, label, move |color| {
                            svg()
                                .data(&data)
                                .size(px(icon_size))
                                .text_color(color)
                                .into_any_element()
                        })
                        .contextual_style(style, focus)
                        .target_size(px(target))
                        .border_width(px(0.0))
                        .visual_inset(px((target - diameter) / 2.0))
                        .corner_radius(px(diameter / 2.0))
                        .focus_outline(
                            px(2.0),
                            px(0.0),
                            px(if facts.style == DesktopWindowStyle::Adwaita {
                                9.0
                            } else {
                                10.0
                            }),
                        )
                        .debug_selector(id)
                        .accept_first_mouse(true)
                        .on_activate(move |_, window, cx| activate(window, cx)),
                    ),
            );
        }
        crate::ModalLayer::window_chrome(controls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_selection_uses_exact_names_and_an_adwaita_fallback() {
        for desktops in ["GNOME", "ubuntu:GNOME", "X-Cinnamon", "NotKDE", ""] {
            assert_eq!(
                DesktopWindowStyle::select(desktops, ""),
                DesktopWindowStyle::Adwaita
            );
        }
        for desktops in ["KDE", "plasma", "KDE:PLASMA"] {
            assert_eq!(
                DesktopWindowStyle::select(desktops, "Adwaita"),
                DesktopWindowStyle::Breeze
            );
        }
        assert_eq!(
            DesktopWindowStyle::select("", "Breeze-Dark"),
            DesktopWindowStyle::Breeze
        );
    }
    #[test]
    fn native_controls_keep_layout_sides_and_window_capabilities() {
        use WindowButton::*;
        let layout = WindowButtonLayout {
            left: [Some(Close), Some(Minimize), None],
            right: [Some(Maximize), None, None],
        };
        assert_eq!(
            buttons(
                WindowControlSide::Left,
                Some(layout),
                gpui::WindowControls::default(),
                true,
                true
            ),
            vec![Close, Minimize]
        );
        assert_eq!(
            buttons(
                WindowControlSide::Right,
                Some(layout),
                gpui::WindowControls::default(),
                true,
                true
            ),
            vec![Maximize]
        );
        assert_eq!(
            buttons(
                WindowControlSide::Left,
                Some(layout),
                gpui::WindowControls::default(),
                false,
                false
            ),
            vec![Close]
        );
        assert!(
            buttons(
                WindowControlSide::Left,
                None,
                gpui::WindowControls::default(),
                true,
                true
            )
            .is_empty()
        );
    }
    #[test]
    fn native_control_state_selects_restore_and_surface_variant() {
        assert_eq!(glyph(WindowButton::Maximize, false), 2);
        assert_eq!(glyph(WindowButton::Maximize, true), 3);
        assert!(window_controls_dark(rgba(0x202020ff)));
        assert!(!window_controls_dark(rgba(0xfafafaff)));
        let (active, _) = paints(DesktopWindowStyle::Adwaita, false, true, true);
        let (inactive, _) = paints(DesktopWindowStyle::Adwaita, false, false, true);
        assert_eq!(
            inactive.normal().background().a,
            active.normal().background().a * 0.5
        );
        assert_eq!(
            inactive.normal().icon_color().a,
            active.normal().icon_color().a * 0.5
        );
        assert_ne!(active.normal(), active.hovered());
        assert_ne!(active.hovered(), active.pressed());
    }
}
