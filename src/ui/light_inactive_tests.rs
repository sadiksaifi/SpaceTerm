use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeColorOverrides, ChromeColors, Color, CompositionCapabilities, SchemeCatalog,
    SystemAppearance,
};

use super::appearance::{ChromeAppearance, settings};

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

#[test]
fn builtin_light_active_segments_preserve_explicit_hover_and_pressed_overrides() {
    let hover = Color::rgb(0xf7f7f7);
    let pressed = Color::rgb(0xf0f0f0);
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..AppearancePreferences::default()
    };
    preferences.chrome.overrides.insert(
        crate::appearance::builtin_light_chrome(),
        ChromeColorOverrides {
            selection_hover_background: Some(hover),
            selection_pressed_background: Some(pressed),
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

    assert_eq!(
        active.segmented_control_colors.selection_hover_background,
        hover
    );
    assert_eq!(
        active.segmented_control_colors.selection_pressed_background,
        pressed
    );
}

fn assert_active_segment_uses_elevated_surface(
    name: &str,
    transparency: f32,
    appearance: &ChromeAppearance,
    reference: &ChromeColors,
    paint: &ChromeColors,
    final_host: Color,
    floating: bool,
) {
    let track = paint.element_background.source_over(final_host);
    let elevated = Color::rgb(0xfdfdfd);
    let materials = if floating {
        appearance.floating_materials
    } else {
        appearance.materials
    };
    let expected_paint = super::appearance::prominent_surface_with(
        materials,
        reference.segmented_track_background,
        elevated,
    );
    for (state, fill, foreground) in [
        (
            "selected",
            paint.selection_background,
            paint.selection_foreground,
        ),
        (
            "selected hover",
            paint.selection_hover_background,
            paint.selection_hover_foreground,
        ),
        (
            "selected pressed",
            paint.selection_pressed_background,
            paint.selection_pressed_foreground,
        ),
    ] {
        assert_eq!(
            fill, expected_paint,
            "active Light {name} {state} segment at {transparency} must preserve the elevated selection material and its alpha"
        );
        assert!(
            foreground.contrast_ratio(fill.source_over(track)) >= 4.5,
            "active Light {name} {state} segment at {transparency} must keep readable content"
        );
    }
}

#[test]
fn builtin_light_active_segments_use_the_elevated_selection_material_on_every_host() {
    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_builtin_light(transparency);
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings_active, _) =
            settings::prepare_variants(&resolved.chrome, active.clone(), inactive);

        for (name, host, reference, paint) in [
            (
                "Window",
                spaceterm_ui::ControlHost::Window,
                &active.colors,
                &active.segmented_control_colors,
            ),
            (
                "TitleBar",
                spaceterm_ui::ControlHost::TitleBar,
                &active.title_bar_controls.reference,
                &active.title_bar_controls.segmented,
            ),
            (
                "Panel",
                spaceterm_ui::ControlHost::Panel,
                &active.panel_controls.reference,
                &active.panel_controls.segmented,
            ),
            (
                "Card",
                spaceterm_ui::ControlHost::Card,
                &active.card_controls.reference,
                &active.card_controls.segmented,
            ),
        ] {
            assert_active_segment_uses_elevated_surface(
                name,
                transparency,
                &active,
                reference,
                paint,
                active.control_host_background(host),
                false,
            );
        }

        let floating_host =
            active.floating_surface(active.floating_colors.elevated_surface_background);
        assert_active_segment_uses_elevated_surface(
            "Floating",
            transparency,
            &active,
            &active.floating_colors,
            &active.floating_segmented_colors,
            floating_host,
            true,
        );

        for (name, host, reference, paint) in [
            (
                "Settings Panel",
                spaceterm_ui::ControlHost::Panel,
                &settings_active.chrome.panel_controls.reference,
                &settings_active.chrome.panel_controls.segmented,
            ),
            (
                "Settings Card",
                spaceterm_ui::ControlHost::Card,
                &settings_active.chrome.card_controls.reference,
                &settings_active.chrome.card_controls.segmented,
            ),
        ] {
            assert_active_segment_uses_elevated_surface(
                name,
                transparency,
                &settings_active.chrome,
                reference,
                paint,
                settings_active.chrome.control_host_background(host),
                false,
            );
        }
    }
}
