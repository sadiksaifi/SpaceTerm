use gpui::{Rgba, px, rgba};
use spaceterm_ui::{CommandPaletteMetrics, CommandPalettePaint, CommandPaletteTheme};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_icons::{ChromeIcons, IconRole};
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

#[cfg(test)]
pub(super) fn theme(colors: &ChromeColors) -> CommandPaletteTheme {
    themed(colors, colors)
}

/// Paints `colors`, resolving row content against the opaque `reference`.
#[cfg(test)]
pub(super) fn themed(reference: &ChromeColors, colors: &ChromeColors) -> CommandPaletteTheme {
    prepared(
        reference,
        colors,
        None,
        &ChromeTypography::default(),
        &ChromeIcons::default(),
        super::control_theme_catalog::OverlayRowPolicy::default(),
    )
}

pub(super) fn prepared(
    reference: &ChromeColors,
    colors: &ChromeColors,
    unfocused_rows: Option<(&ChromeColors, &ChromeColors)>,
    typography: &ChromeTypography,
    icons: &ChromeIcons,
    row_policy: super::control_theme_catalog::OverlayRowPolicy,
) -> CommandPaletteTheme {
    let body = typography.style(TextRole::Body);
    let secondary = typography.style(TextRole::Secondary);
    let section = typography.style(TextRole::Section);
    CommandPaletteTheme::new(
        CommandPalettePaint::new(
            gpui_color(colors.text),
            gpui_color(colors.text_muted),
            gpui_color(colors.text_disabled),
            gpui_color(colors.ghost_element_selected),
            gpui_color(colors.ghost_element_selected_foreground),
            gpui_color(colors.text_accent),
        )
        .rows({
            let rows = super::control_theme_catalog::overlay_list_rows_with_policy(
                reference, colors, row_policy,
            );
            unfocused_rows.map_or(rows, |(unfocused_reference, unfocused_paint)| {
                rows.unfocused_selection(
                    super::control_theme_catalog::overlay_list_rows_with_policy(
                        unfocused_reference,
                        unfocused_paint,
                        row_policy,
                    ),
                )
            })
        })
        .icons(gpui_color(colors.icon), gpui_color(colors.icon_disabled))
        .hover_background(gpui_color(colors.ghost_element_hover))
        .hover_foreground(gpui_color(colors.ghost_element_hover_foreground))
        .section_foreground(gpui_color(colors.text_muted))
        .footer(gpui_color(colors.text_muted), gpui_color(colors.text_muted)),
        CommandPaletteMetrics::new(px(600.0), px(48.0))
            .single_line_row_height(px(32.0))
            .footer_control_padding(px(8.0))
            .panel_geometry(px(480.0), px(52.0))
            .viewport_margin(px(16.0))
            .editor_height(px(42.0))
            .row_spacing(px(12.0), px(18.0), px(10.0))
            .row_line_gap(px(2.0))
            .section_spacing(px(20.0), px(9.0))
            .footer_height(px(30.0))
            .font_sizes(body.size, body.size, secondary.size)
            .section_font_size(section.size)
            .text_geometry(
                body.line_height,
                secondary.line_height,
                section.line_height,
                icons.metrics(IconRole::Row).glyph_size,
            )
            .icon_baseline_center(icons.metrics(IconRole::Row).baseline_center),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}
