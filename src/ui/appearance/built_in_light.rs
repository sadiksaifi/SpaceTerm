//! Surface, boundary, and state policy for the built-in Light definition only.

use crate::appearance::{
    Appearance, ChromeColors, Color, ResolvedChromeAppearance, SurfaceMaterials,
};
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

/// Whether the window uses Light appearance.
pub(super) fn applies(resolved: &ResolvedChromeAppearance) -> bool {
    resolved.appearance == Appearance::Light
}

/// Popup collections use the base surface so selected rows can use the raised surface.
pub(super) fn floating_reference(authored: &ChromeColors) -> ChromeColors {
    let mut source = authored.clone();
    source.elevated_surface_background = authored.background;
    source.floating_presentation()
}

/// Retains the state steps of small controls while their containing surfaces transmit the backdrop.
pub(super) fn control_paints(
    reference: &ChromeColors,
    materials: SurfaceMaterials,
) -> ChromeColors {
    let mut paint = reference.material_presentation(materials);
    macro_rules! retain_state {
        ($($role:ident),+ $(,)?) => { $(
            paint.$role = super::prominent_surface_with(materials, reference.background, reference.$role);
        )+ };
    }
    retain_state!(
        element_background,
        element_hover,
        element_active,
        ghost_element_background,
        ghost_element_hover,
        ghost_element_active
    );
    paint
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

    let selection = source.inactive_selection_background;
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

    if !state.capabilities.show_borders {
        let rim = source.inactive_selection_border;
        colors.selection_border = rim;
        colors.selection_hover_border = rim;
        colors.selection_pressed_border = rim;
        colors.row_selected_border = rim;
        colors.row_selected_hover_border = rim;
        colors.tab_active_border = rim;
    }

    colors
}

fn use_selected_material(colors: &mut ChromeColors, material: Color) {
    colors.selection_background = material;
    colors.selection_hover_background = material;
    colors.selection_pressed_background = material;
}

/// Routes active selected segments through the same prominent material as navigation selection.
pub(super) fn prepare_active_segmented_controls(
    appearance: &mut ChromeAppearance,
    resolved: &ResolvedChromeAppearance,
) {
    if appearance.appearance != Appearance::Light
        || !appearance.active
        || appearance.capabilities.increase_contrast
    {
        return;
    }

    let selected = resolved.colors.selection_background;
    let material = |materials: SurfaceMaterials, reference: &ChromeColors| {
        super::prominent_surface_with(materials, reference.segmented_track_background, selected)
    };

    let window = material(appearance.materials, &appearance.colors);
    let title_bar = material(
        appearance.materials,
        &appearance.title_bar_controls.reference,
    );
    let panel = material(appearance.materials, &appearance.panel_controls.reference);
    let card = material(appearance.materials, &appearance.card_controls.reference);
    let floating = material(appearance.floating_materials, &appearance.floating_colors);
    use_selected_material(&mut appearance.segmented_control_colors, window);
    use_selected_material(&mut appearance.title_bar_controls.segmented, title_bar);
    use_selected_material(&mut appearance.panel_controls.segmented, panel);
    use_selected_material(&mut appearance.card_controls.segmented, card);
    use_selected_material(&mut appearance.floating_segmented_colors, floating);
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
    if appearance.appearance != Appearance::Light || appearance.active {
        return;
    }
    suppress_inactive_segment_hover(&mut appearance.segmented_control_colors);
    suppress_inactive_segment_hover(&mut appearance.title_bar_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.panel_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.card_controls.segmented);
    suppress_inactive_segment_hover(&mut appearance.floating_segmented_colors);
}
