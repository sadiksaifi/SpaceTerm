use gpui::{Rgba, px, rgba};
use spaceterm_ui::{ComboBoxMetrics, ComboBoxPaint, ComboBoxTheme};

use crate::appearance::{ChromeColors, Color};
use crate::ui::chrome_geometry::RadiusRole;
use crate::ui::chrome_icons::{ChromeIcons, IconRole};
use crate::ui::chrome_typography::{ChromeTypography, TextRole};

#[cfg(test)]
pub(super) fn theme(colors: &ChromeColors) -> ComboBoxTheme {
    themed_with_rows(colors, colors, colors)
}

/// Keeps a trigger's host treatment independent from rows on the shared floating material.
#[cfg(test)]
pub(super) fn themed_with_rows(
    reference: &ChromeColors,
    colors: &ChromeColors,
    row_colors: &ChromeColors,
) -> ComboBoxTheme {
    prepared_with_rows(
        reference,
        colors,
        row_colors,
        &ChromeTypography::default(),
        &ChromeIcons::default(),
        super::control_theme_catalog::OverlayRowPolicy::default(),
    )
}

pub(super) fn prepared_with_rows(
    reference: &ChromeColors,
    colors: &ChromeColors,
    row_colors: &ChromeColors,
    typography: &ChromeTypography,
    icons: &ChromeIcons,
    row_policy: super::control_theme_catalog::OverlayRowPolicy,
) -> ComboBoxTheme {
    let body = typography.style(TextRole::Body);
    let secondary = typography.style(TextRole::Secondary);
    ComboBoxTheme::new(
        ComboBoxPaint::new(
            gpui_color(colors.element_foreground),
            gpui_color(colors.text_muted),
            gpui_color(colors.element_disabled_foreground),
            gpui_color(colors.ghost_element_selected),
            gpui_color(colors.ghost_element_selected_foreground),
            gpui_color(colors.element_background),
            gpui_color(colors.element_hover),
            gpui_color(colors.element_border),
            gpui_color(colors.focus_ring),
        )
        .trigger_icon_colors(
            gpui_color(colors.element_icon),
            gpui_color(colors.element_disabled_icon),
        )
        .trigger_state_backgrounds(
            gpui_color(colors.element_active),
            gpui_color(colors.element_disabled),
        )
        .rows(super::control_theme_catalog::overlay_list_rows_with_policy(
            reference, row_colors, row_policy,
        ))
        .hover_background(gpui_color(colors.ghost_element_hover))
        .hover_foreground(gpui_color(colors.ghost_element_hover_foreground)),
        ComboBoxMetrics::new(px(240.0), px(28.0))
            .icon_trigger_size(px(28.0))
            .geometry(px(260.0), px(28.0), px(28.0), px(40.0))
            .spacing(px(8.0), px(18.0), px(6.0))
            .row_gutters(px(16.0), px(18.0), px(4.0), px(6.0))
            .trigger_shape(RadiusRole::Control.pixels())
            .font_sizes(body.size, secondary.size)
            .text_geometry(
                body.line_height,
                secondary.line_height,
                icons.metrics(IconRole::Row).glyph_size,
            )
            .trigger_icon_size(icons.metrics(IconRole::Control).glyph_size)
            .icon_baseline_center(icons.metrics(IconRole::Row).baseline_center),
    )
}

fn gpui_color(color: Color) -> Rgba {
    rgba(color.rgba_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_colors_should_not_replace_only_the_combo_box_surface() {
        let popup = ChromeColors {
            elevated_surface_background: Color::rgb(0x101010),
            border: Color::rgb(0x808080),
            text: Color::rgb(0xffffff),
            text_muted: Color::rgb(0xcccccc),
            text_disabled: Color::rgb(0x777777),
            input_background: Color::rgb(0xffffff),
            input_text: Color::rgb(0x000000),
            input_border: Color::rgb(0x0000ff),
            ..ChromeColors::default()
        };
        let changed_fields = ChromeColors {
            input_background: Color::rgb(0xff00ff),
            input_text: Color::rgb(0x003300),
            input_placeholder: Color::rgb(0x005500),
            input_border: Color::rgb(0x00ff00),
            input_focused_border: Color::rgb(0xffff00),
            input_disabled_background: Color::rgb(0x55ffff),
            ..popup.clone()
        };

        // The bezel retains its existing popup paint contract. Borrowing only the white input
        // fill would put the popup's white label on white while leaving other states unrelated.
        assert_eq!(theme(&popup), theme(&changed_fields));
        assert_ne!(
            theme(&popup),
            theme(&ChromeColors {
                elevated_surface_background: Color::rgb(0x202020),
                ..popup
            })
        );
    }

    #[test]
    fn custom_chrome_row_icons_are_independent_from_trigger_icons() {
        let colors = ChromeColors::default();
        let row_icons = ChromeColors {
            row_hover_icon: Color::rgba(0x12345678),
            ..colors.clone()
        };
        let trigger_icons = ChromeColors {
            element_icon: Color::rgba(0xabcdef98),
            ..colors.clone()
        };
        assert_ne!(theme(&row_icons), theme(&colors));
        assert_ne!(theme(&trigger_icons), theme(&colors));
        assert_ne!(theme(&row_icons), theme(&trigger_icons));
    }

    #[test]
    fn popup_button_frame_uses_the_ordinary_control_family() {
        let colors = ChromeColors::default();
        for changed in [
            ChromeColors {
                element_background: Color::rgb(0x123456),
                ..colors.clone()
            },
            ChromeColors {
                element_hover: Color::rgb(0x234567),
                ..colors.clone()
            },
            ChromeColors {
                element_active: Color::rgb(0x345678),
                ..colors.clone()
            },
            ChromeColors {
                element_disabled: Color::rgb(0x456789),
                ..colors.clone()
            },
            ChromeColors {
                element_border: Color::rgb(0x56789a),
                ..colors.clone()
            },
        ] {
            assert_ne!(theme(&colors), theme(&changed));
        }
        assert_eq!(
            theme(&colors),
            theme(&ChromeColors {
                ghost_element_background: Color::rgb(0xabcdef),
                ..colors
            })
        );
    }
}
