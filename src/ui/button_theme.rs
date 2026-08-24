use gpui::{
    AnyView, App, AppContext as _, Context, IntoElement, ParentElement as _, Render, Rgba,
    SharedString, Styled as _, Window, div, px, rgba,
};
use spaceterm_ui::{
    ButtonMetrics, ButtonPaint, ButtonSizes, ButtonTheme, ButtonVariantStyle, ButtonVariants,
};

use crate::theme::{ACTIVE_THEME, Color};

pub(super) fn theme() -> ButtonTheme {
    ButtonTheme::new(
        ButtonVariants::new(
            variant(
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
            ),
            variant(
                paint(
                    ACTIVE_THEME.element_background,
                    ACTIVE_THEME.text,
                    ACTIVE_THEME.border_transparent,
                ),
                paint(
                    ACTIVE_THEME.element_hover,
                    ACTIVE_THEME.text,
                    ACTIVE_THEME.border_transparent,
                ),
                paint(
                    ACTIVE_THEME.element_active,
                    ACTIVE_THEME.text,
                    ACTIVE_THEME.border_transparent,
                ),
                paint(
                    ACTIVE_THEME.element_disabled,
                    ACTIVE_THEME.text_disabled,
                    ACTIVE_THEME.border_transparent,
                ),
            ),
            outline(),
            variant(
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
            ),
            variant(
                paint(
                    ACTIVE_THEME.error_background,
                    ACTIVE_THEME.error,
                    ACTIVE_THEME.error_border,
                ),
                paint(
                    ACTIVE_THEME.element_hover,
                    ACTIVE_THEME.error,
                    ACTIVE_THEME.error_border,
                ),
                paint(
                    ACTIVE_THEME.element_active,
                    ACTIVE_THEME.error,
                    ACTIVE_THEME.error_border,
                ),
                paint(
                    ACTIVE_THEME.element_disabled,
                    ACTIVE_THEME.text_disabled,
                    ACTIVE_THEME.border_disabled,
                ),
            ),
            variant(
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
            ),
        ),
        ButtonSizes::new(
            ButtonMetrics::new(px(20.0))
                .horizontal_padding(px(4.0))
                .gap(px(4.0))
                .corner_radius(px(4.0))
                .font_size(px(11.0)),
            ButtonMetrics::new(px(24.0))
                .horizontal_padding(px(8.0))
                .gap(px(6.0))
                .corner_radius(px(5.0))
                .font_size(px(12.0)),
            ButtonMetrics::new(px(28.0))
                .horizontal_padding(px(10.0))
                .gap(px(6.0))
                .corner_radius(px(6.0))
                .font_size(px(12.0)),
            ButtonMetrics::new(px(40.0))
                .horizontal_padding(px(12.0))
                .gap(px(8.0))
                .corner_radius(px(6.0))
                .font_size(px(12.0)),
        ),
        gpui_color(ACTIVE_THEME.border_focused),
    )
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

fn outline() -> ButtonVariantStyle {
    variant(
        paint(
            ACTIVE_THEME.element_background,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border,
        ),
        paint(
            ACTIVE_THEME.element_hover,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border,
        ),
        paint(
            ACTIVE_THEME.element_active,
            ACTIVE_THEME.icon,
            ACTIVE_THEME.border,
        ),
        paint(
            ACTIVE_THEME.element_disabled,
            ACTIVE_THEME.icon_disabled,
            ACTIVE_THEME.border_disabled,
        ),
    )
}

fn variant(
    normal: ButtonPaint,
    hovered: ButtonPaint,
    pressed: ButtonPaint,
    disabled: ButtonPaint,
) -> ButtonVariantStyle {
    ButtonVariantStyle::new(normal, hovered, pressed, disabled)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_press_should_preserve_the_normal_border() {
        let outline = outline();

        assert_eq!(outline.normal().border(), outline.pressed().border());
    }
}
