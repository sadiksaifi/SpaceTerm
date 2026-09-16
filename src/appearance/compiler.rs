//! One acyclic, source-local completion program for every Chrome definition.
use super::scheme::ChromeColorOverrides;
use super::{Appearance, ChromeColors, Color};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorProvenance {
    Authored,
    Overridden,
    Derived,
    NeutralFallback,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CompiledChrome {
    pub(crate) colors: ChromeColors,
    pub(crate) readability: Vec<ChromeReadabilityDiagnostic>,
    pub(crate) provenance: BTreeMap<&'static str, ColorProvenance>,
}

/// Content-free readability findings. Explicit authored paint is preserved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChromeReadabilityDiagnostic {
    Text,
    Placeholder,
    Primary,
    Destructive,
    SelectedRow,
    Focus,
    Scrollbar,
}

fn readability_diagnostics(authored: &ChromeColors) -> Vec<ChromeReadabilityDiagnostic> {
    let c = authored.opaque_presentation();
    use ChromeReadabilityDiagnostic::*;
    [
        (Text, c.text, c.background, 4.5),
        (Placeholder, c.input_placeholder, c.input_background, 4.5),
        (Primary, c.primary_foreground, c.primary_background, 4.5),
        (
            Destructive,
            c.destructive_foreground,
            c.destructive_background,
            4.5,
        ),
        (
            SelectedRow,
            c.row_selected_foreground,
            c.row_selected_background,
            4.5,
        ),
        (Focus, c.border_focused, c.background, 3.0),
        (
            Scrollbar,
            c.scrollbar_thumb_background,
            c.panel_background,
            3.0,
        ),
    ]
    .into_iter()
    .filter_map(|(kind, foreground, background, minimum)| {
        let background = background.source_over(c.background.with_alpha(255));
        (foreground
            .source_over(background)
            .contrast_ratio(background)
            < minimum)
            .then_some(kind)
    })
    .collect()
}

/// Overrides precede authored values; missing roles follow the effective dependencies below.
/// Only missing background uses an appearance-specific neutral seed. Missing text chooses a
/// contrasting neutral; all other decisions follow this definition, never a built-in parent.
pub(crate) fn compile_chrome(
    appearance: Appearance,
    authored: &ChromeColorOverrides,
    overrides: &ChromeColorOverrides,
) -> CompiledChrome {
    let mut provenance = BTreeMap::new();
    macro_rules! resolve {
        ($role:ident, $fallback:expr) => {{
            let (value, origin) = if let Some(value) = overrides.$role {
                (value, ColorProvenance::Overridden)
            } else if let Some(value) = authored.$role {
                (value, ColorProvenance::Authored)
            } else {
                (
                    $fallback,
                    if matches!(stringify!($role), "background" | "text") {
                        ColorProvenance::NeutralFallback
                    } else {
                        ColorProvenance::Derived
                    },
                )
            };
            provenance.insert(stringify!($role), origin);
            value
        }};
    }
    let background = resolve!(
        background,
        match appearance {
            Appearance::Dark => Color::rgb(0x202020),
            Appearance::Light => Color::rgb(0xfafafa),
        }
    );
    // Foundation native windows have an opaque root backing of the root's own RGB.
    // Derivation and opaque_presentation use identical backing rules.
    let root_surface = background.with_alpha(255);
    let contrast = |foreground: Color, surface: Color, minimum| {
        contrast(foreground, surface.source_over(root_surface), minimum)
    };
    let text = resolve!(text, contrast(Color::rgb(0x202020), background, 4.5));
    let panel_background = resolve!(panel_background, background);
    let elevated_surface_background = resolve!(elevated_surface_background, background);
    let title_bar_background = resolve!(title_bar_background, background);
    let title_bar_inactive_background = resolve!(title_bar_inactive_background, background);
    let text_accent = resolve!(text_accent, text);
    let text_secondary = resolve!(
        text_secondary,
        contrast(text.mix(background, 0.25), background, 4.5)
    );
    let text_muted = resolve!(
        text_muted,
        contrast(text.mix(background, 0.35), background, 4.5)
    );
    let text_placeholder = resolve!(text_placeholder, text_muted);
    let text_disabled = resolve!(text_disabled, text.mix(background, 0.55));
    let link_text = resolve!(link_text, contrast(text_accent, background, 4.5));
    let link_text_hover = resolve!(link_text_hover, link_text);
    let icon = resolve!(icon, text);
    let icon_muted = resolve!(icon_muted, text_muted);
    let icon_disabled = resolve!(icon_disabled, text_disabled);
    let border = resolve!(border, text.mix(background, 0.7));
    let border_variant = resolve!(border_variant, border);
    let border_focused = resolve!(border_focused, contrast(text_accent, background, 3.0));
    let border_selected = resolve!(border_selected, border_focused);
    let border_disabled = resolve!(border_disabled, border);
    let border_transparent = resolve!(border_transparent, Color::rgba(0));
    let element_background = resolve!(element_background, background);
    let element_hover = resolve!(element_hover, element_background.mix(text, 0.08));
    let element_active = resolve!(element_active, element_background.mix(text, 0.14));
    let element_selected = resolve!(element_selected, background.mix(text_accent, 0.16));
    let element_disabled = resolve!(element_disabled, background);
    let element_foreground = resolve!(element_foreground, contrast(text, element_background, 4.5));
    let element_hover_foreground =
        resolve!(element_hover_foreground, contrast(text, element_hover, 4.5));
    let element_active_foreground = resolve!(
        element_active_foreground,
        contrast(text, element_active, 4.5)
    );
    let element_disabled_foreground = resolve!(element_disabled_foreground, text_disabled);
    let ghost_element_background = resolve!(ghost_element_background, Color::rgba(0));
    let ghost_element_hover = resolve!(
        ghost_element_hover,
        ghost_element_background
            .source_over(root_surface)
            .mix(text, 0.08)
    );
    let ghost_element_active = resolve!(
        ghost_element_active,
        ghost_element_background
            .source_over(root_surface)
            .mix(text, 0.14)
    );
    let ghost_element_selected = resolve!(ghost_element_selected, element_selected);
    let ghost_element_disabled = resolve!(ghost_element_disabled, Color::rgba(0));
    let ghost_element_foreground = resolve!(
        ghost_element_foreground,
        contrast(text, ghost_element_background, 4.5)
    );
    let ghost_element_hover_foreground = resolve!(
        ghost_element_hover_foreground,
        contrast(text, ghost_element_hover, 4.5)
    );
    let ghost_element_active_foreground = resolve!(
        ghost_element_active_foreground,
        contrast(text, ghost_element_active, 4.5)
    );
    let ghost_element_selected_foreground = resolve!(
        ghost_element_selected_foreground,
        contrast(text, ghost_element_selected, 4.5)
    );
    let ghost_element_disabled_foreground =
        resolve!(ghost_element_disabled_foreground, text_disabled);
    let sidebar_focus = resolve!(sidebar_focus, border_focused);
    let info = resolve!(info, text_accent);
    let success = resolve!(success, text_accent);
    let warning = resolve!(warning, text_accent);
    let error = resolve!(error, text_accent);
    let info_background = resolve!(info_background, info.multiply_opacity(0x1a));
    let info_border = resolve!(info_border, info);
    let success_background = resolve!(success_background, success.multiply_opacity(0x1a));
    let success_border = resolve!(success_border, success);
    let warning_background = resolve!(warning_background, warning.multiply_opacity(0x1a));
    let warning_border = resolve!(warning_border, warning);
    let error_background = resolve!(error_background, error.multiply_opacity(0x1a));
    let error_border = resolve!(error_border, error);
    let input_background = resolve!(input_background, background);
    let input_surface = input_background.source_over(panel_background.source_over(root_surface));
    let input_text = resolve!(input_text, contrast(text, input_surface, 4.5));
    let input_placeholder = resolve!(
        input_placeholder,
        contrast(text_placeholder, input_surface, 4.5)
    );
    let input_disabled_background = resolve!(input_disabled_background, input_background);
    let input_disabled_text = resolve!(input_disabled_text, text_disabled);
    let input_caret = resolve!(input_caret, input_text);
    let input_selection_background = resolve!(
        input_selection_background,
        text_accent.multiply_opacity(0x66)
    );
    let input_selection_foreground = resolve!(
        input_selection_foreground,
        contrast(
            input_text,
            input_selection_background.source_over(input_surface),
            4.5
        )
    );
    let input_border = resolve!(input_border, border);
    let input_focused_border = resolve!(
        input_focused_border,
        contrast(border_focused, input_surface, 3.0)
    );
    let input_invalid_border = resolve!(input_invalid_border, contrast(error, input_surface, 3.0));
    let input_disabled_border = resolve!(input_disabled_border, border_disabled);
    let modal_scrim = resolve!(modal_scrim, background.multiply_opacity(0x99));
    let scrollbar_track = resolve!(scrollbar_track, Color::rgba(0));
    let scrollbar_track_border = resolve!(scrollbar_track_border, Color::rgba(0));
    let scrollbar_thumb_background = resolve!(
        scrollbar_thumb_background,
        contrast(text_muted, panel_background, 3.0)
    );
    let scrollbar_thumb_border = resolve!(scrollbar_thumb_border, Color::rgba(0));
    let scrollbar_thumb_hover_background = resolve!(
        scrollbar_thumb_hover_background,
        contrast(
            scrollbar_thumb_background.mix(text, 0.15),
            panel_background,
            3.0
        )
    );
    let scrollbar_thumb_active_background = resolve!(
        scrollbar_thumb_active_background,
        contrast(
            scrollbar_thumb_hover_background.mix(text, 0.2),
            panel_background,
            3.0
        )
    );
    let resize_idle = resolve!(resize_idle, border);
    let resize_focused = resolve!(resize_focused, border_focused);
    let resize_hovered = resolve!(resize_hovered, border_focused);
    let resize_dragged = resolve!(resize_dragged, border_selected);
    let resize_disabled = resolve!(resize_disabled, border_disabled);
    let shadow = resolve!(shadow, Color::rgba(0x00000033));
    let primary_background = resolve!(primary_background, text_accent);
    let primary_foreground = resolve!(
        primary_foreground,
        contrast(text, primary_background.source_over(background), 4.5)
    );
    let primary_icon = resolve!(primary_icon, primary_foreground);
    let primary_border = resolve!(primary_border, primary_background);
    let primary_hover_background = resolve!(
        primary_hover_background,
        primary_background.mix(primary_foreground, 0.10)
    );
    let primary_hover_foreground = resolve!(
        primary_hover_foreground,
        contrast(text, primary_hover_background.source_over(background), 4.5)
    );
    let primary_hover_icon = resolve!(primary_hover_icon, primary_hover_foreground);
    let primary_hover_border = resolve!(primary_hover_border, primary_hover_background);
    let primary_pressed_background = resolve!(
        primary_pressed_background,
        primary_background.mix(primary_foreground, 0.18)
    );
    let primary_pressed_foreground = resolve!(
        primary_pressed_foreground,
        contrast(
            text,
            primary_pressed_background.source_over(background),
            4.5
        )
    );
    let primary_pressed_icon = resolve!(primary_pressed_icon, primary_pressed_foreground);
    let primary_pressed_border = resolve!(primary_pressed_border, primary_pressed_background);
    let primary_disabled_background = resolve!(
        primary_disabled_background,
        primary_background.mix(background, 0.65)
    );
    let primary_disabled_foreground = resolve!(primary_disabled_foreground, text_disabled);
    let primary_disabled_icon = resolve!(primary_disabled_icon, primary_disabled_foreground);
    let primary_disabled_border = resolve!(primary_disabled_border, primary_disabled_background);
    let destructive_background = resolve!(destructive_background, error);
    let destructive_foreground = resolve!(
        destructive_foreground,
        contrast(text, destructive_background.source_over(background), 4.5)
    );
    let destructive_icon = resolve!(destructive_icon, destructive_foreground);
    let destructive_border = resolve!(destructive_border, destructive_background);
    let destructive_hover_background = resolve!(
        destructive_hover_background,
        destructive_background.mix(destructive_foreground, 0.10)
    );
    let destructive_hover_foreground = resolve!(
        destructive_hover_foreground,
        contrast(
            text,
            destructive_hover_background.source_over(background),
            4.5
        )
    );
    let destructive_hover_icon = resolve!(destructive_hover_icon, destructive_hover_foreground);
    let destructive_hover_border = resolve!(destructive_hover_border, destructive_hover_background);
    let destructive_pressed_background = resolve!(
        destructive_pressed_background,
        destructive_background.mix(destructive_foreground, 0.18)
    );
    let destructive_pressed_foreground = resolve!(
        destructive_pressed_foreground,
        contrast(
            text,
            destructive_pressed_background.source_over(background),
            4.5
        )
    );
    let destructive_pressed_icon =
        resolve!(destructive_pressed_icon, destructive_pressed_foreground);
    let destructive_pressed_border =
        resolve!(destructive_pressed_border, destructive_pressed_background);
    let destructive_disabled_background = resolve!(
        destructive_disabled_background,
        destructive_background.mix(background, 0.65)
    );
    let destructive_disabled_foreground = resolve!(destructive_disabled_foreground, text_disabled);
    let destructive_disabled_icon =
        resolve!(destructive_disabled_icon, destructive_disabled_foreground);
    let destructive_disabled_border =
        resolve!(destructive_disabled_border, destructive_disabled_background);
    let selection_background = resolve!(selection_background, element_selected);
    let selection_foreground = resolve!(
        selection_foreground,
        contrast(text, selection_background.source_over(background), 4.5)
    );
    let selection_icon = resolve!(selection_icon, selection_foreground);
    let selection_border = resolve!(selection_border, selection_background);
    let selection_hover_background = resolve!(
        selection_hover_background,
        selection_background.mix(text, 0.06)
    );
    let selection_hover_foreground = resolve!(
        selection_hover_foreground,
        contrast(
            text,
            selection_hover_background.source_over(background),
            4.5
        )
    );
    let selection_hover_icon = resolve!(selection_hover_icon, selection_hover_foreground);
    let selection_hover_border = resolve!(selection_hover_border, selection_hover_background);
    let selection_pressed_background = resolve!(
        selection_pressed_background,
        selection_background.mix(text, 0.18)
    );
    let selection_pressed_foreground = resolve!(
        selection_pressed_foreground,
        contrast(
            text,
            selection_pressed_background.source_over(background),
            4.5
        )
    );
    let selection_pressed_icon = resolve!(selection_pressed_icon, selection_pressed_foreground);
    let selection_pressed_border = resolve!(selection_pressed_border, selection_pressed_background);
    let selection_disabled_background = resolve!(
        selection_disabled_background,
        selection_background.mix(background, 0.65)
    );
    let selection_disabled_foreground = resolve!(selection_disabled_foreground, text_disabled);
    let selection_disabled_icon = resolve!(selection_disabled_icon, selection_disabled_foreground);
    let selection_disabled_border =
        resolve!(selection_disabled_border, selection_disabled_background);
    let toggle_off_background = resolve!(toggle_off_background, input_background);
    let toggle_off_mark = resolve!(
        toggle_off_mark,
        contrast(text, toggle_off_background.source_over(background), 4.5)
    );
    let toggle_off_border = resolve!(
        toggle_off_border,
        contrast(border, toggle_off_background.source_over(background), 3.0)
    );
    let toggle_off_label = resolve!(toggle_off_label, text);
    let toggle_off_hover_background = resolve!(
        toggle_off_hover_background,
        toggle_off_background.mix(toggle_off_mark, 0.08)
    );
    let toggle_off_hover_mark = resolve!(
        toggle_off_hover_mark,
        contrast(
            text,
            toggle_off_hover_background.source_over(background),
            4.5
        )
    );
    let toggle_off_hover_border = resolve!(
        toggle_off_hover_border,
        contrast(
            border,
            toggle_off_hover_background.source_over(background),
            3.0
        )
    );
    let toggle_off_hover_label = resolve!(toggle_off_hover_label, text);
    let toggle_off_pressed_background = resolve!(
        toggle_off_pressed_background,
        toggle_off_background.mix(toggle_off_mark, 0.16)
    );
    let toggle_off_pressed_mark = resolve!(
        toggle_off_pressed_mark,
        contrast(
            text,
            toggle_off_pressed_background.source_over(background),
            4.5
        )
    );
    let toggle_off_pressed_border = resolve!(
        toggle_off_pressed_border,
        contrast(
            border,
            toggle_off_pressed_background.source_over(background),
            3.0
        )
    );
    let toggle_off_pressed_label = resolve!(toggle_off_pressed_label, text);
    let toggle_off_disabled_background = resolve!(
        toggle_off_disabled_background,
        toggle_off_background.mix(background, 0.65)
    );
    let toggle_off_disabled_mark = resolve!(
        toggle_off_disabled_mark,
        contrast(
            text,
            toggle_off_disabled_background.source_over(background),
            4.5
        )
    );
    let toggle_off_disabled_border = resolve!(toggle_off_disabled_border, border_disabled);
    let toggle_off_disabled_label = resolve!(toggle_off_disabled_label, text_disabled);
    let toggle_on_background = resolve!(toggle_on_background, text_accent);
    let toggle_on_mark = resolve!(
        toggle_on_mark,
        contrast(text, toggle_on_background.source_over(background), 4.5)
    );
    let toggle_on_border = resolve!(
        toggle_on_border,
        contrast(border, toggle_on_background.source_over(background), 3.0)
    );
    let toggle_on_label = resolve!(toggle_on_label, text);
    let toggle_on_hover_background = resolve!(
        toggle_on_hover_background,
        toggle_on_background.mix(toggle_on_mark, 0.08)
    );
    let toggle_on_hover_mark = resolve!(
        toggle_on_hover_mark,
        contrast(
            text,
            toggle_on_hover_background.source_over(background),
            4.5
        )
    );
    let toggle_on_hover_border = resolve!(
        toggle_on_hover_border,
        contrast(
            border,
            toggle_on_hover_background.source_over(background),
            3.0
        )
    );
    let toggle_on_hover_label = resolve!(toggle_on_hover_label, text);
    let toggle_on_pressed_background = resolve!(
        toggle_on_pressed_background,
        toggle_on_background.mix(toggle_on_mark, 0.16)
    );
    let toggle_on_pressed_mark = resolve!(
        toggle_on_pressed_mark,
        contrast(
            text,
            toggle_on_pressed_background.source_over(background),
            4.5
        )
    );
    let toggle_on_pressed_border = resolve!(
        toggle_on_pressed_border,
        contrast(
            border,
            toggle_on_pressed_background.source_over(background),
            3.0
        )
    );
    let toggle_on_pressed_label = resolve!(toggle_on_pressed_label, text);
    let toggle_on_disabled_background = resolve!(
        toggle_on_disabled_background,
        toggle_on_background.mix(background, 0.65)
    );
    let toggle_on_disabled_mark = resolve!(
        toggle_on_disabled_mark,
        contrast(
            text,
            toggle_on_disabled_background.source_over(background),
            4.5
        )
    );
    let toggle_on_disabled_border = resolve!(toggle_on_disabled_border, border_disabled);
    let toggle_on_disabled_label = resolve!(toggle_on_disabled_label, text_disabled);
    let row_background = resolve!(row_background, background);
    let row_foreground = resolve!(
        row_foreground,
        contrast(text, row_background.source_over(background), 4.5)
    );
    let row_secondary = resolve!(
        row_secondary,
        contrast(text_secondary, row_background.source_over(background), 4.5)
    );
    let row_icon = resolve!(row_icon, row_foreground);
    let row_match = resolve!(
        row_match,
        contrast(text_accent, row_background.source_over(background), 4.5)
    );
    let row_border = resolve!(row_border, border_transparent);
    let row_hover_background = resolve!(row_hover_background, row_background.mix(text, 0.08));
    let row_hover_foreground = resolve!(
        row_hover_foreground,
        contrast(text, row_hover_background.source_over(background), 4.5)
    );
    let row_hover_secondary = resolve!(
        row_hover_secondary,
        contrast(
            text_secondary,
            row_hover_background.source_over(background),
            4.5
        )
    );
    let row_hover_icon = resolve!(row_hover_icon, row_hover_foreground);
    let row_hover_match = resolve!(
        row_hover_match,
        contrast(
            text_accent,
            row_hover_background.source_over(background),
            4.5
        )
    );
    let row_hover_border = resolve!(row_hover_border, border_transparent);
    let row_selected_background = resolve!(row_selected_background, selection_background);
    let row_selected_foreground = resolve!(
        row_selected_foreground,
        contrast(text, row_selected_background.source_over(background), 4.5)
    );
    let row_selected_secondary = resolve!(
        row_selected_secondary,
        contrast(
            text_secondary,
            row_selected_background.source_over(background),
            4.5
        )
    );
    let row_selected_icon = resolve!(row_selected_icon, row_selected_foreground);
    let row_selected_match = resolve!(
        row_selected_match,
        contrast(
            text_accent,
            row_selected_background.source_over(background),
            4.5
        )
    );
    let row_selected_border = resolve!(row_selected_border, border_transparent);
    let row_selected_hover_background = resolve!(
        row_selected_hover_background,
        row_selected_background.mix(text, 0.06)
    );
    let row_selected_hover_foreground = resolve!(
        row_selected_hover_foreground,
        contrast(
            text,
            row_selected_hover_background.source_over(background),
            4.5
        )
    );
    let row_selected_hover_secondary = resolve!(
        row_selected_hover_secondary,
        contrast(
            text_secondary,
            row_selected_hover_background.source_over(background),
            4.5
        )
    );
    let row_selected_hover_icon = resolve!(row_selected_hover_icon, row_selected_hover_foreground);
    let row_selected_hover_match = resolve!(
        row_selected_hover_match,
        contrast(
            text_accent,
            row_selected_hover_background.source_over(background),
            4.5
        )
    );
    let row_selected_hover_border = resolve!(row_selected_hover_border, border_transparent);
    let element_icon = resolve!(element_icon, element_foreground);
    let element_hover_icon = resolve!(element_hover_icon, element_hover_foreground);
    let element_active_icon = resolve!(element_active_icon, element_active_foreground);
    let element_disabled_icon = resolve!(element_disabled_icon, element_disabled_foreground);
    let ghost_element_icon = resolve!(ghost_element_icon, ghost_element_foreground);
    let ghost_element_hover_icon =
        resolve!(ghost_element_hover_icon, ghost_element_hover_foreground);
    let ghost_element_active_icon =
        resolve!(ghost_element_active_icon, ghost_element_active_foreground);
    let ghost_element_disabled_icon = resolve!(
        ghost_element_disabled_icon,
        ghost_element_disabled_foreground
    );
    let badge_background = resolve!(badge_background, panel_background);
    let badge_foreground = resolve!(
        badge_foreground,
        contrast(text_secondary, badge_background, 4.5)
    );
    let preview_background = resolve!(preview_background, elevated_surface_background);
    let preview_foreground = resolve!(
        preview_foreground,
        contrast(text_secondary, preview_background, 4.5)
    );
    let tab_active_background = resolve!(tab_active_background, element_selected);
    let tab_inactive_background = resolve!(tab_inactive_background, title_bar_background);
    let tab_active_foreground = resolve!(
        tab_active_foreground,
        contrast(text, tab_active_background, 4.5)
    );
    let tab_inactive_foreground = resolve!(
        tab_inactive_foreground,
        contrast(text_secondary, tab_inactive_background, 4.5)
    );
    let tab_inactive_selected_background =
        resolve!(tab_inactive_selected_background, tab_active_background);
    let tab_inactive_selected_foreground = resolve!(
        tab_inactive_selected_foreground,
        contrast(text, tab_inactive_selected_background, 4.5)
    );
    let element_border = resolve!(element_border, border_transparent);
    let element_hover_border = resolve!(element_hover_border, border_transparent);
    let element_active_border = resolve!(element_active_border, border_transparent);
    let element_disabled_border = resolve!(element_disabled_border, border_transparent);
    let ghost_element_border = resolve!(ghost_element_border, border_transparent);
    let ghost_element_hover_border = resolve!(ghost_element_hover_border, border_transparent);
    let ghost_element_active_border = resolve!(ghost_element_active_border, border_transparent);
    let ghost_element_disabled_border = resolve!(ghost_element_disabled_border, border_disabled);
    let outline_border = resolve!(outline_border, border);
    let outline_hover_border = resolve!(outline_hover_border, border);
    let outline_pressed_border = resolve!(outline_pressed_border, border);
    let outline_disabled_border = resolve!(outline_disabled_border, border_disabled);
    let tab_hover_background = resolve!(tab_hover_background, row_hover_background);
    let tab_hover_foreground = resolve!(
        tab_hover_foreground,
        contrast(text, tab_hover_background, 4.5)
    );
    let tab_hover_icon = resolve!(tab_hover_icon, tab_hover_foreground);
    let tab_active_icon = resolve!(tab_active_icon, tab_active_foreground);
    let tab_inactive_icon = resolve!(tab_inactive_icon, tab_inactive_foreground);
    let tab_inactive_selected_icon =
        resolve!(tab_inactive_selected_icon, tab_inactive_selected_foreground);
    // The Active Tab carries the selected-row hierarchy in its own roles: a rim that stays put, and a
    // hover that gains weight from the Tab's material rather than from a list row's.
    let tab_active_border = resolve!(tab_active_border, border_transparent);
    let tab_active_hover_background = resolve!(
        tab_active_hover_background,
        tab_active_background.mix(text, 0.06)
    );
    let tab_active_hover_foreground = resolve!(
        tab_active_hover_foreground,
        contrast(
            tab_active_foreground,
            tab_active_hover_background.source_over(background),
            4.5
        )
    );
    // Close rests on the selected-hover fill whenever the pointer is anywhere over the Active Tab,
    // so its glyph follows the hovered title and keeps a non-text contrast against that fill.
    let tab_active_hover_icon = resolve!(
        tab_active_hover_icon,
        contrast(
            tab_active_hover_foreground,
            tab_active_hover_background.source_over(background),
            3.0
        )
    );
    let tab_inactive_selected_border = resolve!(tab_inactive_selected_border, tab_active_border);
    // The mark between two inactive Tabs is its own decision rather than a control outline or a
    // full-length divider. Missing, it takes the inactive title a step back into the bar it rests
    // on, so it follows the scheme's own title weight: seen as a short hairline, quieter than text.
    let tab_separator = resolve!(
        tab_separator,
        tab_inactive_foreground.mix(title_bar_background.source_over(root_surface), 0.6)
    );
    let link_text_pressed = resolve!(link_text_pressed, link_text_hover);
    let link_text_disabled = resolve!(link_text_disabled, text_disabled);
    let colors = ChromeColors {
        background,
        panel_background,
        elevated_surface_background,
        title_bar_background,
        title_bar_inactive_background,
        tab_active_background,
        tab_inactive_background,
        text,
        text_secondary,
        text_muted,
        text_placeholder,
        text_disabled,
        text_accent,
        link_text,
        link_text_hover,
        link_text_pressed,
        link_text_disabled,
        icon,
        icon_muted,
        icon_disabled,
        border,
        border_variant,
        border_focused,
        border_selected,
        border_disabled,
        border_transparent,
        element_background,
        element_hover,
        element_active,
        element_selected,
        element_disabled,
        element_foreground,
        element_hover_foreground,
        element_active_foreground,
        element_disabled_foreground,
        ghost_element_background,
        ghost_element_hover,
        ghost_element_active,
        ghost_element_selected,
        ghost_element_disabled,
        ghost_element_foreground,
        ghost_element_hover_foreground,
        ghost_element_active_foreground,
        ghost_element_selected_foreground,
        ghost_element_disabled_foreground,
        sidebar_focus,
        info,
        info_background,
        success,
        warning,
        warning_background,
        warning_border,
        error,
        error_background,
        error_border,
        input_text,
        input_placeholder,
        input_disabled_text,
        input_caret,
        input_selection_background,
        input_selection_foreground,
        input_background,
        input_disabled_background,
        input_border,
        input_focused_border,
        input_invalid_border,
        modal_scrim,
        scrollbar_track,
        scrollbar_track_border,
        scrollbar_thumb_background,
        scrollbar_thumb_border,
        scrollbar_thumb_hover_background,
        resize_idle,
        resize_focused,
        resize_hovered,
        resize_dragged,
        resize_disabled,
        shadow,
        primary_background,
        primary_foreground,
        primary_icon,
        primary_border,
        primary_hover_background,
        primary_hover_foreground,
        primary_hover_icon,
        primary_hover_border,
        primary_pressed_background,
        primary_pressed_foreground,
        primary_pressed_icon,
        primary_pressed_border,
        primary_disabled_background,
        primary_disabled_foreground,
        primary_disabled_icon,
        primary_disabled_border,
        destructive_background,
        destructive_foreground,
        destructive_icon,
        destructive_border,
        destructive_hover_background,
        destructive_hover_foreground,
        destructive_hover_icon,
        destructive_hover_border,
        destructive_pressed_background,
        destructive_pressed_foreground,
        destructive_pressed_icon,
        destructive_pressed_border,
        destructive_disabled_background,
        destructive_disabled_foreground,
        destructive_disabled_icon,
        destructive_disabled_border,
        selection_background,
        selection_foreground,
        selection_icon,
        selection_border,
        selection_hover_background,
        selection_hover_foreground,
        selection_hover_icon,
        selection_hover_border,
        selection_pressed_background,
        selection_pressed_foreground,
        selection_pressed_icon,
        selection_pressed_border,
        selection_disabled_background,
        selection_disabled_foreground,
        selection_disabled_icon,
        selection_disabled_border,
        toggle_off_background,
        toggle_off_mark,
        toggle_off_border,
        toggle_off_label,
        toggle_off_hover_background,
        toggle_off_hover_mark,
        toggle_off_hover_border,
        toggle_off_hover_label,
        toggle_off_pressed_background,
        toggle_off_pressed_mark,
        toggle_off_pressed_border,
        toggle_off_pressed_label,
        toggle_off_disabled_background,
        toggle_off_disabled_mark,
        toggle_off_disabled_border,
        toggle_off_disabled_label,
        toggle_on_background,
        toggle_on_mark,
        toggle_on_border,
        toggle_on_label,
        toggle_on_hover_background,
        toggle_on_hover_mark,
        toggle_on_hover_border,
        toggle_on_hover_label,
        toggle_on_pressed_background,
        toggle_on_pressed_mark,
        toggle_on_pressed_border,
        toggle_on_pressed_label,
        toggle_on_disabled_background,
        toggle_on_disabled_mark,
        toggle_on_disabled_border,
        toggle_on_disabled_label,
        row_background,
        row_foreground,
        row_secondary,
        row_icon,
        row_match,
        row_border,
        row_hover_background,
        row_hover_foreground,
        row_hover_secondary,
        row_hover_icon,
        row_hover_match,
        row_hover_border,
        row_selected_background,
        row_selected_foreground,
        row_selected_secondary,
        row_selected_icon,
        row_selected_match,
        row_selected_border,
        row_selected_hover_background,
        row_selected_hover_foreground,
        row_selected_hover_secondary,
        row_selected_hover_icon,
        row_selected_hover_match,
        row_selected_hover_border,
        element_icon,
        element_hover_icon,
        element_active_icon,
        element_disabled_icon,
        ghost_element_icon,
        ghost_element_hover_icon,
        ghost_element_active_icon,
        ghost_element_disabled_icon,
        input_disabled_border,
        badge_background,
        badge_foreground,
        preview_background,
        preview_foreground,
        tab_active_foreground,
        tab_inactive_foreground,
        tab_inactive_selected_background,
        tab_inactive_selected_foreground,
        scrollbar_thumb_active_background,
        success_background,
        success_border,
        info_border,
        element_border,
        element_hover_border,
        element_active_border,
        element_disabled_border,
        ghost_element_border,
        ghost_element_hover_border,
        ghost_element_active_border,
        ghost_element_disabled_border,
        outline_border,
        outline_hover_border,
        outline_pressed_border,
        outline_disabled_border,
        tab_hover_background,
        tab_hover_foreground,
        tab_hover_icon,
        tab_active_icon,
        tab_inactive_icon,
        tab_inactive_selected_icon,
        tab_active_border,
        tab_active_hover_background,
        tab_active_hover_foreground,
        tab_active_hover_icon,
        tab_inactive_selected_border,
        tab_separator,
    };
    CompiledChrome {
        readability: readability_diagnostics(&colors),
        colors,
        provenance,
    }
}

/// Preserve the proposed color when readable, otherwise choose the better neutral.
fn contrast(foreground: Color, background: Color, minimum: f64) -> Color {
    if foreground
        .source_over(background)
        .contrast_ratio(background)
        >= minimum
    {
        return foreground;
    }
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    if dark.contrast_ratio(background) >= light.contrast_ratio(background) {
        dark
    } else {
        light
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SemanticPaint {
    pub(crate) background: Color,
    pub(crate) foreground: Color,
    pub(crate) icon: Color,
    pub(crate) border: Color,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaptionPaint {
    pub(crate) background: Color,
    pub(crate) foreground: Color,
    pub(crate) secondary: Color,
    pub(crate) icon: Color,
    pub(crate) focus: Color,
    pub(crate) attention: Color,
    pub(crate) error: Color,
    pub(crate) control: SemanticPaint,
    pub(crate) control_hover: SemanticPaint,
    pub(crate) control_pressed: SemanticPaint,
    pub(crate) control_disabled: SemanticPaint,
}

impl ChromeColors {
    /// Pane Caption context is an opaque Terminal surface, including program-controlled colors.
    /// It does not mutate the theme. All caption controls share this contextual policy.
    pub(crate) fn caption(&self, surface: Color, focused: bool) -> CaptionPaint {
        let surface = surface.source_over(self.background.with_alpha(255));
        let foreground = contrast(
            if focused {
                self.text_secondary
            } else {
                self.text_muted
            },
            surface,
            4.5,
        );
        let paint = |background: Color, proposed: Color| {
            let background = background.source_over(surface);
            let foreground = contrast(proposed, background, 4.5);
            SemanticPaint {
                background,
                foreground,
                icon: foreground,
                border: Color::rgba(0),
            }
        };
        CaptionPaint {
            background: surface,
            foreground,
            secondary: contrast(self.text_muted, surface, 4.5),
            icon: foreground,
            focus: contrast(self.border_focused, surface, 3.0),
            attention: contrast(self.warning, surface, 4.5),
            error: contrast(self.error, surface, 4.5),
            control: paint(surface, foreground),
            control_hover: paint(surface.mix(foreground, 0.10), foreground),
            control_pressed: paint(surface.mix(foreground, 0.18), foreground),
            control_disabled: paint(surface, self.text_disabled),
        }
    }
}

/// Status marks that must stay readable on every surface one control can rest on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StatusPaint {
    pub(crate) attention: Color,
    pub(crate) error: Color,
}

impl ChromeColors {
    /// Resolves the semantic status colors against every surface a control shows them on.
    ///
    /// Each surface is composed over the opaque Chrome background. A semantic color that already
    /// reads on all of them is kept; otherwise black or white, whichever reads best everywhere.
    pub(crate) fn status(&self, surfaces: &[Color]) -> StatusPaint {
        let base = self.background.with_alpha(255);
        let surfaces = surfaces
            .iter()
            .map(|surface| surface.source_over(base))
            .collect::<Vec<_>>();
        StatusPaint {
            attention: contrast_on_all(self.warning, &surfaces, 3.0),
            error: contrast_on_all(self.error, &surfaces, 3.0),
        }
    }
}

fn contrast_on_all(foreground: Color, surfaces: &[Color], minimum: f64) -> Color {
    let worst = |color: Color| {
        surfaces
            .iter()
            .map(|surface| color.source_over(*surface).contrast_ratio(*surface))
            .fold(f64::INFINITY, f64::min)
    };
    if worst(foreground) >= minimum {
        return foreground;
    }
    let dark = Color::rgb(0x000000);
    let light = Color::rgb(0xffffff);
    if worst(dark) >= worst(light) {
        dark
    } else {
        light
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        AppearanceGeneration, AppearancePreferences, AvailableFonts, SchemeCatalog,
        SystemAppearance, builtin,
    };
    use super::*;

    #[test]
    fn sparse_definitions_are_total_deterministic_and_source_local() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let authored = ChromeColorOverrides {
                background: Some(Color::rgb(0x010203)),
                text: Some(Color::rgb(0xfefefe)),
                ..Default::default()
            };
            let result = compile_chrome(appearance, &authored, &Default::default());
            assert_eq!(
                result,
                compile_chrome(appearance, &authored, &Default::default())
            );
            assert_eq!(result.colors.input_background, Color::rgb(0x010203));
            assert_eq!(result.colors.link_text, Color::rgb(0xfefefe));
            assert_eq!(result.provenance["background"], ColorProvenance::Authored);
            assert_eq!(
                result.provenance["input_background"],
                ColorProvenance::Derived
            );
            assert!(result.colors.validate().is_ok());
        }
    }

    #[test]
    fn dependencies_follow_overrides_while_authored_values_retain_intent() {
        let authored = ChromeColorOverrides {
            background: Some(Color::rgb(0x123456)),
            error: Some(Color::rgb(0xff0000)),
            ..Default::default()
        };
        let overrides = ChromeColorOverrides {
            background: Some(Color::rgb(0x654321)),
            error: Some(Color::rgb(0x00ff00)),
            ..Default::default()
        };
        let result = compile_chrome(Appearance::Dark, &authored, &overrides);
        assert_eq!(result.colors.modal_scrim, Color::rgba(0x65432199));
        assert_eq!(result.colors.error_background, Color::rgba(0x00ff001a));
        assert_eq!(result.provenance["background"], ColorProvenance::Overridden);
        let authored = ChromeColorOverrides {
            modal_scrim: Some(Color::rgba(0x11223344)),
            error_background: Some(Color::rgba(0x55667788)),
            ..authored
        };
        let result = compile_chrome(Appearance::Dark, &authored, &overrides);
        assert_eq!(result.colors.modal_scrim, Color::rgba(0x11223344));
        assert_eq!(result.colors.error_background, Color::rgba(0x55667788));
        assert_eq!(result.provenance["modal_scrim"], ColorProvenance::Authored);
    }

    #[test]
    fn emphasis_selection_status_and_static_surfaces_are_independent() {
        let authored = builtin::chrome_definition(Appearance::Dark);
        let baseline = compile_chrome(Appearance::Dark, &authored, &Default::default()).colors;
        let changed = compile_chrome(
            Appearance::Dark,
            &authored,
            &ChromeColorOverrides {
                primary_background: Some(Color::rgb(0xff00ff)),
                destructive_background: Some(Color::rgb(0x00ffff)),
                element_active: Some(Color::rgb(0xffff00)),
                ..Default::default()
            },
        )
        .colors;
        assert_ne!(changed.primary_background, baseline.primary_background);
        assert_eq!(changed.selection_background, baseline.selection_background);
        assert_eq!(changed.badge_background, baseline.badge_background);
        assert_eq!(changed.preview_background, baseline.preview_background);
        assert_eq!(changed.error_background, baseline.error_background);
        assert_eq!(
            changed.row_selected_background,
            baseline.row_selected_background
        );
    }

    #[test]
    fn semantic_selection_and_row_overrides_drive_their_missing_hover_states() {
        let authored = super::super::builtin::chrome_definition(Appearance::Dark);
        let baseline = compile_chrome(Appearance::Dark, &authored, &Default::default());
        let overrides = ChromeColorOverrides {
            selection_background: Some(Color::rgb(0xff0000)),
            row_selected_background: Some(Color::rgb(0x00ff00)),
            row_background: Some(Color::rgb(0x0000ff)),
            ..Default::default()
        };
        let changed = compile_chrome(Appearance::Dark, &authored, &overrides);
        assert_ne!(
            changed.colors.selection_hover_background,
            baseline.colors.selection_hover_background
        );
        assert_ne!(
            changed.colors.row_selected_hover_background,
            baseline.colors.row_selected_hover_background
        );
        assert_ne!(
            changed.colors.row_hover_background,
            baseline.colors.row_hover_background
        );
        assert!(
            changed.colors.selection_hover_background.r
                > changed.colors.selection_hover_background.g
        );
        assert!(
            changed.colors.row_selected_hover_background.g
                > changed.colors.row_selected_hover_background.r
        );
        let authored = ChromeColorOverrides {
            selection_hover_background: Some(Color::rgb(0x123456)),
            row_selected_hover_background: Some(Color::rgb(0x654321)),
            ..authored
        };
        let preserved = compile_chrome(Appearance::Dark, &authored, &overrides);
        assert_eq!(
            preserved.colors.selection_hover_background,
            Color::rgb(0x123456)
        );
        assert_eq!(
            preserved.colors.row_selected_hover_background,
            Color::rgb(0x654321)
        );
    }

    #[test]
    fn active_tab_hover_icon_is_completed_against_its_rendered_fill_and_preserves_authored_intent()
    {
        let authored = ChromeColorOverrides {
            background: Some(Color::rgb(0x111111)),
            text: Some(Color::rgb(0xffffff)),
            tab_active_hover_background: Some(Color::rgba(0xffffffcc)),
            tab_active_hover_foreground: Some(Color::rgb(0x000000)),
            ..Default::default()
        };
        let completed = compile_chrome(Appearance::Dark, &authored, &Default::default());
        let rendered_fill = completed
            .colors
            .tab_active_hover_background
            .source_over(completed.colors.background);
        assert!(
            completed
                .colors
                .tab_active_hover_icon
                .contrast_ratio(rendered_fill)
                >= 3.0
        );
        assert_eq!(
            completed.provenance["tab_active_hover_icon"],
            ColorProvenance::Derived
        );

        let authored_icon = Color::rgb(0xff00ff);
        let authored = ChromeColorOverrides {
            tab_active_hover_icon: Some(authored_icon),
            ..authored
        };
        let preserved = compile_chrome(Appearance::Dark, &authored, &Default::default());
        assert_eq!(preserved.colors.tab_active_hover_icon, authored_icon);
        assert_eq!(
            preserved.provenance["tab_active_hover_icon"],
            ColorProvenance::Authored
        );
    }

    /// The Tab separator and an outlined control's ring are separate authorable decisions.
    #[test]
    fn tab_separator_is_its_own_role_independent_of_outlined_controls() {
        let authored = builtin::chrome_definition(Appearance::Dark);
        let baseline = compile_chrome(Appearance::Dark, &authored, &Default::default());
        assert_eq!(
            baseline.provenance["tab_separator"],
            ColorProvenance::Authored
        );

        let outline = Color::rgb(0xff00ff);
        let retuned_outline = compile_chrome(
            Appearance::Dark,
            &ChromeColorOverrides {
                outline_border: Some(outline),
                ..authored.clone()
            },
            &Default::default(),
        );
        assert_eq!(retuned_outline.colors.outline_border, outline);
        assert_eq!(
            retuned_outline.colors.tab_separator,
            baseline.colors.tab_separator
        );

        let separator = Color::rgb(0x00ffff);
        let retuned_separator = compile_chrome(
            Appearance::Dark,
            &authored,
            &ChromeColorOverrides {
                tab_separator: Some(separator),
                ..Default::default()
            },
        );
        assert_eq!(retuned_separator.colors.tab_separator, separator);
        assert_eq!(
            retuned_separator.provenance["tab_separator"],
            ColorProvenance::Overridden
        );
        assert_eq!(
            ChromeColors {
                tab_separator: baseline.colors.tab_separator,
                ..retuned_separator.colors
            },
            baseline.colors,
            "retuning the Tab separator should move no other role"
        );

        // A sparse scheme that outlines its controls loudly still derives a quiet separator.
        let sparse = ChromeColorOverrides {
            background: Some(Color::rgb(0x101010)),
            text: Some(Color::rgb(0xeeeeee)),
            outline_border: Some(Color::rgb(0xffffff)),
            ..Default::default()
        };
        let derived = compile_chrome(Appearance::Dark, &sparse, &Default::default());
        assert_eq!(
            derived.provenance["tab_separator"],
            ColorProvenance::Derived
        );
        assert_ne!(derived.colors.tab_separator, derived.colors.outline_border);
    }

    /// A missing separator follows the scheme's own inactive title a step back into the bar, so it
    /// is visible as a short hairline yet quieter than the titles it divides, whether the window is
    /// focused or not.
    #[test]
    fn derived_tab_separator_is_visible_but_quiet_on_both_title_bar_surfaces() {
        for (appearance, background, text) in [
            (Appearance::Dark, 0x010203, 0xfefefe),
            (Appearance::Dark, 0x141415, 0xcdcdcd),
            (Appearance::Dark, 0x2d2a3e, 0xd8d4f0),
            (Appearance::Light, 0xffffff, 0x000000),
            (Appearance::Light, 0xf6f1e4, 0x3b3226),
            (Appearance::Light, 0xdcdcdc, 0x202020),
        ] {
            let authored = ChromeColorOverrides {
                background: Some(Color::rgb(background)),
                text: Some(Color::rgb(text)),
                ..Default::default()
            };
            let c = compile_chrome(appearance, &authored, &Default::default())
                .colors
                .opaque_presentation();
            for bar in [c.title_bar_background, c.title_bar_inactive_background] {
                let separator = c.tab_separator.source_over(bar).contrast_ratio(bar);
                let title = c.tab_inactive_foreground.contrast_ratio(bar);
                assert!(
                    (1.4..=3.0).contains(&separator) && separator < title,
                    "{appearance:?} {background:06x}/{text:06x}: separator {separator:.2} \
                     against title {title:.2}"
                );
            }
        }
    }

    #[test]
    fn translucent_fields_derive_against_the_same_backing_that_is_rendered() {
        for (panel, input) in [
            (Color::rgb(0xffffff), Color::rgba(0)),
            (Color::rgba(0xff000080), Color::rgba(0x0000ff80)),
        ] {
            let authored = ChromeColorOverrides {
                background: Some(Color::rgb(0xffffff)),
                text: Some(Color::rgb(0x000000)),
                panel_background: Some(panel),
                input_background: Some(input),
                input_selection_background: Some(Color::rgba(0xffffff80)),
                ..Default::default()
            };
            let result = compile_chrome(Appearance::Light, &authored, &Default::default());
            assert_eq!(result.colors.input_background, input);
            let rendered = result.colors.opaque_presentation();
            assert_eq!(
                rendered.input_background,
                input.source_over(panel.source_over(Color::rgb(0xffffff)))
            );
            assert!(
                rendered
                    .input_text
                    .contrast_ratio(rendered.input_background)
                    >= 4.5
            );
            assert!(
                rendered
                    .input_placeholder
                    .contrast_ratio(rendered.input_background)
                    >= 4.5
            );
            assert!(
                rendered
                    .input_focused_border
                    .contrast_ratio(rendered.input_background)
                    >= 3.0
            );
            assert!(
                rendered.input_selection_foreground.contrast_ratio(
                    rendered
                        .input_selection_background
                        .source_over(rendered.input_background)
                ) >= 4.5
            );
            assert_eq!(rendered.ghost_element_background.a, 0);
        }
    }

    #[test]
    fn translucent_interaction_and_static_surfaces_have_readable_derived_foregrounds() {
        let authored = ChromeColorOverrides {
            background: Some(Color::rgb(0xffffff)),
            text: Some(Color::rgb(0)),
            element_background: Some(Color::rgba(0)),
            preview_background: Some(Color::rgba(0)),
            badge_background: Some(Color::rgba(0)),
            tab_active_background: Some(Color::rgba(0)),
            ..Default::default()
        };
        let result = compile_chrome(Appearance::Light, &authored, &Default::default());
        let p = result.colors.opaque_presentation();
        for (foreground, surface) in [
            (p.element_foreground, p.element_background),
            (p.preview_foreground, p.preview_background),
            (p.badge_foreground, p.badge_background),
            (p.tab_active_foreground, p.tab_active_background),
            (p.tab_active_hover_foreground, p.tab_active_hover_background),
        ] {
            assert!(foreground.contrast_ratio(surface) >= 4.5);
        }
        let authored = ChromeColorOverrides {
            input_selection_foreground: Some(Color::rgb(0x123456)),
            ..authored
        };
        assert_eq!(
            compile_chrome(Appearance::Light, &authored, &Default::default())
                .colors
                .input_selection_foreground,
            Color::rgb(0x123456)
        );
    }

    #[test]
    fn minimal_no_accent_actions_and_toggles_retain_default_interaction_feedback() {
        let authored = ChromeColorOverrides {
            background: Some(Color::rgb(0x101010)),
            text: Some(Color::rgb(0xeeeeee)),
            ..Default::default()
        };
        let c = compile_chrome(Appearance::Dark, &authored, &Default::default()).colors;
        for (normal, hover, pressed) in [
            (
                c.primary_background,
                c.primary_hover_background,
                c.primary_pressed_background,
            ),
            (
                c.destructive_background,
                c.destructive_hover_background,
                c.destructive_pressed_background,
            ),
            (
                c.toggle_on_background,
                c.toggle_on_hover_background,
                c.toggle_on_pressed_background,
            ),
        ] {
            assert_ne!(normal, hover);
            assert_ne!(hover, pressed);
        }
        let authored = ChromeColorOverrides {
            primary_hover_background: Some(c.primary_background),
            ..authored
        };
        assert_eq!(
            compile_chrome(Appearance::Dark, &authored, &Default::default())
                .colors
                .primary_hover_background,
            c.primary_background
        );
    }

    #[test]
    fn complete_authored_paints_are_preserved_even_when_unreadable() {
        let baseline = builtin::chrome_base(Appearance::Light);
        let mut authored = ChromeColorOverrides::complete(&baseline);
        authored.primary_foreground = Some(Color::rgb(0x123456));
        authored.primary_background = Some(Color::rgb(0x123456));
        let result = compile_chrome(
            Appearance::Light,
            &authored,
            &ChromeColorOverrides {
                text_accent: Some(Color::rgb(0xabcdef)),
                ..Default::default()
            },
        );
        assert!(
            result
                .readability
                .contains(&ChromeReadabilityDiagnostic::Primary)
        );
        assert_eq!(result.colors.primary_foreground, Color::rgb(0x123456));
        assert_eq!(result.colors.primary_background, Color::rgb(0x123456));
        assert_eq!(
            result.colors.selection_background,
            baseline.selection_background
        );
    }

    #[test]
    fn builtins_pass_supported_text_and_control_contrast_pairs() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let c = builtin::chrome_base(appearance);
            for (foreground, surface) in [
                (c.text, c.background),
                (c.text_secondary, c.panel_background),
                (c.text_muted, c.elevated_surface_background),
                (c.input_placeholder, c.input_background),
                (c.preview_foreground, c.preview_background),
                (c.badge_foreground, c.badge_background),
                (c.primary_foreground, c.primary_background),
                (c.primary_hover_foreground, c.primary_hover_background),
                (c.primary_pressed_foreground, c.primary_pressed_background),
                (c.destructive_foreground, c.destructive_background),
                (
                    c.destructive_hover_foreground,
                    c.destructive_hover_background,
                ),
                (
                    c.destructive_pressed_foreground,
                    c.destructive_pressed_background,
                ),
                (c.row_selected_foreground, c.row_selected_background),
                (c.row_selected_secondary, c.row_selected_background),
                (c.row_selected_match, c.row_selected_background),
                (
                    c.row_selected_hover_foreground,
                    c.row_selected_hover_background,
                ),
                (
                    c.row_selected_hover_secondary,
                    c.row_selected_hover_background,
                ),
                (c.row_selected_hover_match, c.row_selected_hover_background),
                (c.toggle_on_mark, c.toggle_on_background),
                (c.toggle_on_hover_mark, c.toggle_on_hover_background),
                (c.toggle_on_pressed_mark, c.toggle_on_pressed_background),
                (c.toggle_off_mark, c.toggle_off_background),
                (c.toggle_off_hover_mark, c.toggle_off_hover_background),
                (c.toggle_off_pressed_mark, c.toggle_off_pressed_background),
                (
                    c.tab_inactive_selected_foreground,
                    c.tab_inactive_selected_background,
                ),
                (c.tab_active_hover_foreground, c.tab_active_hover_background),
            ] {
                assert!(
                    foreground.source_over(surface).contrast_ratio(surface) >= 4.5,
                    "{appearance:?} {foreground:?} {surface:?}"
                );
            }
            for (indicator, surface) in [
                (c.border_focused, c.background),
                (c.border_focused, c.elevated_surface_background),
                (c.input_focused_border, c.input_background),
                (c.scrollbar_thumb_background, c.panel_background),
                (c.scrollbar_thumb_hover_background, c.panel_background),
                (c.scrollbar_thumb_active_background, c.panel_background),
                (c.toggle_off_border, c.toggle_off_background),
                (c.toggle_off_hover_border, c.toggle_off_hover_background),
                (c.toggle_off_pressed_border, c.toggle_off_pressed_background),
            ] {
                assert!(
                    indicator.source_over(surface).contrast_ratio(surface) >= 3.0,
                    "{appearance:?} {indicator:?} {surface:?}"
                );
            }
        }
    }

    #[test]
    fn captions_resolve_all_control_states_on_opposite_and_program_surfaces() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin::chrome_base(appearance);
            for surface in [
                Color::rgb(0x141415),
                Color::rgb(0xfbfbfc),
                Color::rgb(0xff00ff),
                Color::rgb(0x777777),
                Color::rgb(0),
            ] {
                for focused in [false, true] {
                    let caption = colors.caption(surface, focused);
                    assert_eq!(caption.background, surface);
                    for text in [
                        caption.foreground,
                        caption.secondary,
                        caption.icon,
                        caption.attention,
                        caption.error,
                    ] {
                        assert!(text.contrast_ratio(surface) >= 4.5);
                    }
                    assert!(caption.focus.contrast_ratio(surface) >= 3.0);
                    for paint in [
                        caption.control,
                        caption.control_hover,
                        caption.control_pressed,
                    ] {
                        assert!(paint.foreground.contrast_ratio(paint.background) >= 4.5);
                        assert!(paint.icon.contrast_ratio(paint.background) >= 4.5);
                    }
                }
            }
        }
    }

    #[test]
    fn status_marks_read_on_every_tab_surface_they_rest_on() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            let colors = builtin::chrome_base(appearance);
            let base = colors.background.with_alpha(255);
            for surfaces in [
                vec![
                    colors.tab_active_background,
                    colors.tab_active_hover_background,
                    colors.tab_inactive_background,
                    colors.tab_hover_background,
                ],
                // A surface painted in the warning color itself still gets a readable mark.
                vec![Color::rgb(0xfbfbfc), Color::rgb(0xe0a75c)],
            ] {
                let status = colors.status(&surfaces);
                for mark in [status.attention, status.error] {
                    for surface in &surfaces {
                        let surface = surface.source_over(base);
                        assert!(
                            mark.contrast_ratio(surface) >= 3.0,
                            "{appearance:?} {mark:?} {surface:?}"
                        );
                    }
                }
            }
            let resting = [colors.tab_active_background, colors.tab_inactive_background];
            if resting
                .iter()
                .all(|surface| colors.warning.contrast_ratio(surface.source_over(base)) >= 3.0)
            {
                assert_eq!(colors.status(&resting).attention, colors.warning);
            }
        }
    }

    #[test]
    fn live_builtin_overrides_recompile_dependencies_and_preserve_terminal() {
        let catalog = SchemeCatalog::default();
        let mut preferences = AppearancePreferences::default();
        let before = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        preferences.chrome.overrides.insert(
            before.chrome.effective_scheme.clone(),
            ChromeColorOverrides {
                background: Some(Color::rgb(0x123456)),
                ..Default::default()
            },
        );
        let after = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        assert_eq!(after.chrome.colors.modal_scrim, Color::rgba(0x12345699));
        assert_eq!(before.terminal, after.terminal);
        assert_eq!(
            after.chrome.provenance["background"],
            ColorProvenance::Overridden
        );
    }

    #[test]
    fn schema_lists_exact_registry_and_agrees_on_chrome_alpha_and_null() {
        let schema: serde_json::Value = serde_json::from_str(include_str!(
            "../../docs/schema/color-scheme-definitions-v1.schema.json"
        ))
        .unwrap();
        let roles = schema["$defs"]["chromeColors"]["propertyNames"]["enum"]
            .as_array()
            .unwrap();
        let resolved = compile_chrome(Appearance::Dark, &Default::default(), &Default::default());
        assert_eq!(roles.len(), resolved.provenance.len());
        for role in roles {
            let role = role.as_str().unwrap();
            assert!(resolved.provenance.contains_key(role));
            let value = serde_json::json!({role:"#12345680"});
            let overrides: ChromeColorOverrides = serde_json::from_value(value).unwrap();
            assert_eq!(overrides.validate().is_ok(), role != "border_transparent");
            assert!(
                serde_json::from_value::<ChromeColorOverrides>(serde_json::json!({role: null}))
                    .is_err()
            );
        }
        assert!(
            serde_json::from_value::<ChromeColorOverrides>(
                serde_json::json!({"border_transparent":"#1230"})
            )
            .unwrap()
            .validate()
            .is_ok()
        );
    }
}
