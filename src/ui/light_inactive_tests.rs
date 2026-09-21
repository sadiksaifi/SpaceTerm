use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeColorOverrides, Color, CompositionCapabilities, SchemeCatalog, SystemAppearance,
};

use super::appearance::ChromeAppearance;

fn resolve_builtin_light(transparency: f32) -> crate::appearance::ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..AppearancePreferences::default()
    };
    preferences.background.transparency = transparency;
    SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light)
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .expect("built-in Light should resolve")
}

#[test]
fn builtin_light_inactive_navigation_and_segments_keep_raised_polarity() {
    let inactive_selection = Color::rgb(0xf4f4f4);
    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_builtin_light(transparency);
        let (_, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);

        for (role, host, fill, hover) in [
            (
                "Tab",
                inactive.colors.title_bar_inactive_background,
                inactive.colors.tab_active_background,
                inactive.colors.tab_active_hover_background,
            ),
            (
                "sidebar row",
                inactive.panel_controls.reference.row_background,
                inactive.panel_controls.reference.row_selected_background,
                inactive
                    .panel_controls
                    .reference
                    .row_selected_hover_background,
            ),
        ] {
            assert_eq!(fill, inactive_selection, "{role} at {transparency}");
            assert_eq!(hover, fill, "inactive {role} must suppress hover");
            assert!(
                fill.r > host.r,
                "inactive Light {role} at {transparency} must stay raised over {host:?}"
            );
        }

        for (name, host, colors) in [
            (
                "Window",
                spaceterm_ui::ControlHost::Window,
                &inactive.segmented_control_colors,
            ),
            (
                "Panel",
                spaceterm_ui::ControlHost::Panel,
                &inactive.panel_controls.segmented,
            ),
            (
                "Card",
                spaceterm_ui::ControlHost::Card,
                &inactive.card_controls.segmented,
            ),
        ] {
            let host = inactive.control_host_background(host);
            let track = colors.element_background.source_over(host);
            let selected = colors.selection_background.source_over(track);
            let hovered = colors.selection_hover_background.source_over(track);
            assert!(
                selected.r > track.r,
                "inactive Light {name} segment at {transparency} must stay raised over {track:?}"
            );
            assert_eq!(
                hovered, selected,
                "inactive Light {name} segment at {transparency} must suppress hover"
            );
        }
    }
}

#[test]
fn builtin_light_floating_selection_preserves_user_overrides_equal_to_the_surface() {
    let selected = Color::rgb(0xfafafa);
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..AppearancePreferences::default()
    };
    preferences.chrome.overrides.insert(
        crate::appearance::builtin_light_chrome(),
        ChromeColorOverrides {
            row_selected_background: Some(selected),
            row_selected_hover_background: Some(selected),
            ..ChromeColorOverrides::default()
        },
    );
    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light),
            &AvailableFonts::default(),
        )
        .expect("overridden built-in Light should resolve");
    let active = ChromeAppearance::prepare(&resolved.chrome);

    assert_eq!(active.floating_colors.row_selected_background, selected);
    assert_eq!(
        active.floating_colors.row_selected_hover_background,
        selected
    );
}
