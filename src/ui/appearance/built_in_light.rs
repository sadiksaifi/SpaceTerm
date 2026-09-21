//! Surface, boundary, and state policy for the built-in Light definition only.
//! Custom Light schemes retain the shared preparation path.

use crate::appearance::{ChromeColors, Color, ColorProvenance, ResolvedChromeAppearance};
use crate::ui::chrome_state::ChromeStatePolicy;

use super::{ChromeAppearance, separator::SeparatorBand};

/// Functional rules: row rules inside a card, a menu or a list, and the Settings footer rule.
pub(super) const RULE_BAND: SeparatorBand = SeparatorBand {
    floor: 1.12,
    ceiling: 1.22,
};

/// Card, Pane, floating-shell, and sidebar edges are stronger than internal rules.
pub(super) const SURFACE_BAND: SeparatorBand = SeparatorBand {
    floor: 1.20,
    ceiling: 1.30,
};

/// Ordinary controls need a stronger edge when their fill matches a card or popup host.
pub(crate) const CONTROL_EDGE: Color = Color::rgba(0x0000002b);

/// Hovered and pressed controls use the strong boundary rung.
pub(crate) const INTERACTION_EDGE: Color = Color::rgba(0x00000040);

/// Disabled controls retain their role fill and use the quietest boundary rung.
pub(crate) const DISABLED_EDGE: Color = Color::rgba(0x0000000a);

/// The rim of a selected segmented option and of a switch thumb, both of which carry a shadow too.
pub(crate) const SELECTED_EDGE: Color = Color::rgba(0x0000001a);

/// Popup rows darken on selection because their host already uses the raised content color.
const FLOATING_ROW_SELECTED: Color = Color::rgb(0xebebeb);
const FLOATING_ROW_SELECTED_HOVER: Color = Color::rgb(0xe6e6e6);

/// Whether the prepared appearance is the built-in Light definition, including user overrides on it.
pub(super) fn applies(resolved: &ResolvedChromeAppearance) -> bool {
    resolved.effective_scheme == crate::appearance::builtin_light_chrome()
}

/// Replaces built-in popup defaults without changing user overrides, including equal-color ones.
pub(super) fn prepare_floating_selection(
    reference: &mut ChromeColors,
    resolved: &ResolvedChromeAppearance,
) {
    if matches!(
        resolved.provenance.get("row_selected_background"),
        Some(ColorProvenance::Authored)
    ) {
        reference.row_selected_background = FLOATING_ROW_SELECTED;
    }
    if matches!(
        resolved.provenance.get("row_selected_hover_background"),
        Some(ColorProvenance::Authored)
    ) {
        reference.row_selected_hover_background = FLOATING_ROW_SELECTED_HOVER;
    }
}

/// Applies the built-in Light window-state policy after the shared state compiler.
///
/// The shared inactive resolver may choose the dark endpoint when an almost-white selection is
/// too close to its host. Built-in Light instead owns an explicit inactive selection on the same
/// raised side as the active state. Increase Contrast keeps the shared resolver's stronger result.
pub(super) fn prepare_state_colors(
    state: ChromeStatePolicy,
    source: &ChromeColors,
    host: Color,
) -> ChromeColors {
    let mut colors = state.colors(source, host);
    if state.active || state.capabilities.increase_contrast {
        return colors;
    }

    let selection = source.tab_inactive_selected_background;
    colors.element_selected = selection;
    colors.ghost_element_selected = selection;
    colors.selection_background = selection;
    colors.selection_hover_background = selection;
    colors.selection_pressed_background = selection;
    colors.row_selected_background = selection;
    colors.row_selected_hover_background = selection;
    colors.navigation_selected_background = selection;
    colors.tab_active_background = selection;
    colors.tab_active_hover_background = selection;
    colors.tab_inactive_selected_background = selection;

    if !state.capabilities.show_borders {
        let rim = source.tab_inactive_selected_border;
        colors.selection_border = rim;
        colors.selection_hover_border = rim;
        colors.selection_pressed_border = rim;
        colors.row_selected_border = rim;
        colors.row_selected_hover_border = rim;
        colors.tab_active_border = rim;
        colors.tab_inactive_selected_border = rim;
    }

    colors
}

/// Keeps the final inactive segmented paint non-interactive after independent state resolution.
fn suppress_inactive_segment_hover(colors: &mut ChromeColors) {
    colors.selection_hover_background = colors.selection_background;
    colors.selection_hover_foreground = colors.selection_foreground;
    colors.selection_hover_icon = colors.selection_icon;
    colors.selection_hover_border = colors.selection_border;
}

/// Publishes one non-interactive selected paint after disabled activity reconciliation.
pub(super) fn finalize_inactive_segmented_controls(appearance: &mut ChromeAppearance) {
    if !appearance.built_in_light || appearance.active {
        return;
    }
    suppress_inactive_segment_hover(&mut appearance.segmented_control_colors);
    suppress_inactive_segment_hover(&mut appearance.title_bar_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.panel_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.card_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.floating_segmented_colors);
}
