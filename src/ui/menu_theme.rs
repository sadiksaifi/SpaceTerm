use gpui::{Rgba, px, rgba};
use spaceterm_ui::{MenuMetrics, MenuPaint, MenuSizes, MenuTheme};

use crate::appearance::{ChromeColors, Color};

pub(super) fn theme(colors: &ChromeColors) -> MenuTheme {
    let paint = MenuPaint::new(
        gpui_color(colors.elevated_surface_background),
        gpui_color(colors.border),
        gpui_color(colors.text),
        gpui_color(colors.icon),
        gpui_color(colors.text_disabled),
        gpui_color(colors.ghost_element_selected),
        gpui_color(colors.ghost_element_selected_foreground),
        gpui_color(colors.error),
        gpui_color(colors.border),
    )
    .rows(super::control_theme_catalog::list_rows(colors))
    .destructive_rows(destructive_rows(colors))
    .hover_background(gpui_color(colors.ghost_element_hover))
    .hover_foreground(gpui_color(colors.ghost_element_hover_foreground))
    .trigger(
        gpui_color(colors.ghost_element_background),
        gpui_color(colors.ghost_element_hover),
        gpui_color(colors.border_transparent),
    )
    .focus_border(gpui_color(colors.border_focused));

    MenuTheme::new(
        paint,
        MenuSizes::new(metrics(196.0), metrics(208.0), metrics(240.0)),
    )
    .shadow(super::appearance::control_shadow(colors, false))
}

fn destructive_rows(colors: &ChromeColors) -> spaceterm_ui::ListRowPaints {
    use spaceterm_ui::{ButtonVariant, ListRowPaint, ListRowPaints};
    let button = super::button_theme::theme(colors).paints(ButtonVariant::Destructive);
    let row = |paint: spaceterm_ui::ButtonPaint| {
        ListRowPaint::new(
            paint.background(),
            paint.foreground(),
            paint.foreground(),
            paint.icon_color(),
            paint.foreground(),
            paint.border(),
        )
    };
    ListRowPaints::new(
        row(button.normal()),
        row(button.hovered()),
        row(button.hovered()),
        row(button.pressed()),
        row(button.disabled()),
    )
}

fn metrics(width: f32) -> MenuMetrics {
    MenuMetrics::new(px(width), px(26.0))
        .trigger_height(px(28.0))
        .horizontal_padding(px(6.0))
        .indicator_width(px(16.0))
        .gap(px(6.0))
        .corner_radius(px(8.0))
        .border_width(px(1.0))
        .font_sizes(px(12.0), px(11.0))
        .panel_spacing(px(3.0), px(2.0))
        .decoration_metrics(px(14.0), px(1.0))
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
