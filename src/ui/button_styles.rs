use gpui::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Pixels, Render, Rgba,
    SharedString, Styled as _, Window, div, px, rgba,
};
use spaceterm_ui::{ButtonPaint, ButtonStyle};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn ghost_icon(height: Pixels, corner_radius: Pixels) -> ButtonStyle {
    ButtonStyle::new(
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_hover,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_active,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_disabled,
            ACTIVE_THEME.icon_disabled,
            ACTIVE_THEME.border_transparent,
        ),
        gpui_color(ACTIVE_THEME.border_focused),
    )
    .height(height)
    .corner_radius(corner_radius)
}

pub(super) fn action() -> ButtonStyle {
    ButtonStyle::new(
        paint(
            ACTIVE_THEME.element_active,
            ACTIVE_THEME.text,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.element_hover,
            ACTIVE_THEME.text,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.element_selected,
            ACTIVE_THEME.text,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.element_disabled,
            ACTIVE_THEME.text_disabled,
            ACTIVE_THEME.border_transparent,
        ),
        gpui_color(ACTIVE_THEME.border_focused),
    )
    .height(px(24.0))
    .horizontal_padding(px(8.0))
    .corner_radius(px(5.0))
    .font_size(px(12.0))
}

pub(super) fn text_link() -> ButtonStyle {
    ButtonStyle::new(
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.link_text_hover,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.link_text_hover,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.text_accent,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.text_disabled,
            ACTIVE_THEME.border_transparent,
        ),
        gpui_color(ACTIVE_THEME.border_focused),
    )
    .height(px(16.0))
    .horizontal_padding(px(0.0))
    .gap(px(0.0))
    .corner_radius(px(0.0))
    .font_size(px(12.0))
}

pub(super) fn sidebar_action() -> ButtonStyle {
    ButtonStyle::new(
        paint(
            ACTIVE_THEME.ghost_element_background,
            ACTIVE_THEME.text_muted,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_selected,
            ACTIVE_THEME.text,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_active,
            ACTIVE_THEME.text,
            ACTIVE_THEME.border_transparent,
        ),
        paint(
            ACTIVE_THEME.ghost_element_disabled,
            ACTIVE_THEME.text_disabled,
            ACTIVE_THEME.border_transparent,
        ),
        gpui_color(ACTIVE_THEME.border_focused),
    )
    .height(px(40.0))
    .horizontal_padding(px(12.0))
    .gap(px(8.0))
    .corner_radius(px(0.0))
    .font_size(px(12.0))
}

pub(super) fn tooltip(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
    cx.new(|_| ButtonTooltip { text: text.into() }).into()
}

struct ButtonTooltip {
    text: SharedString,
}

impl Render for ButtonTooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .max_w(px(280.0))
            .px(px(8.0))
            .py(px(5.0))
            .rounded(px(5.0))
            .border(px(1.0))
            .border_color(gpui_color(ACTIVE_THEME.border))
            .bg(gpui_color(ACTIVE_THEME.elevated_surface_background))
            .text_size(px(11.0))
            .text_color(gpui_color(ACTIVE_THEME.text))
            .child(self.text.clone())
    }
}

fn paint(background: Color, foreground: Color, border: Color) -> ButtonPaint {
    ButtonPaint::new(
        gpui_color(background),
        gpui_color(foreground),
        gpui_color(border),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
