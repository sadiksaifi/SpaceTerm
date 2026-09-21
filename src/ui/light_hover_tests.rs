use super::appearance::ChromeAppearance;
use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts, Color,
    CompositionCapabilities, SchemeCatalog, SystemAppearance,
};

fn resolve_light(transparency: f32) -> crate::appearance::ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
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
        .unwrap()
}

fn light(transparency: f32) -> ChromeAppearance {
    ChromeAppearance::prepare(&resolve_light(transparency).chrome)
}

#[test]
fn light_popup_uses_base_host_and_raised_selection() {
    let appearance = light(0.0);
    let colors = &appearance.floating_colors;
    assert_eq!(colors.elevated_surface_background, Color::rgb(0xe5e5e5));
    for selected in [
        colors.row_selected_background,
        colors.row_selected_hover_background,
        appearance
            .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
            .row_selected_background,
    ] {
        assert_eq!(selected, Color::rgb(0xfafafa));
    }
    let shell = appearance
        .floating_surfaces()
        .shell(spaceterm_ui::FloatingRole::Popover);
    let tone = Color::rgba(u32::from(shell.backdrop_tone()));
    let wash = Color::rgba(u32::from(shell.material()));
    assert_eq!(
        wash.source_over(tone.source_over(Color::rgb(0))),
        Color::rgb(0xe5e5e5)
    );
}

#[test]
fn light_unselected_navigation_hover_remains_visible_through_materials() {
    for transparency in [0.0, 0.35, 1.0] {
        let appearance = light(transparency);
        for (name, host, hover) in [
            (
                "Tab",
                appearance.colors.title_bar_background,
                appearance.colors.tab_hover_background,
            ),
            (
                "sidebar",
                appearance.colors.panel_background,
                appearance.panel_controls.reference.row_hover_background,
            ),
        ] {
            let chip = super::selection_chip::ChipPaint {
                fill: None,
                rim: None,
                hover_fill: Some(hover),
                hover_rim: None,
            }
            .raised_on(&appearance, host);
            let hovered = chip.hover_fill.unwrap().source_over(host);
            assert!(
                hovered.contrast_ratio(host) >= 1.10,
                "{name} hover at {transparency} disappears: {hovered:?} over {host:?}"
            );
        }
    }
}

#[test]
fn light_unselected_control_hover_has_a_visible_step_on_each_host() {
    for transparency in [0.0, 0.35, 1.0] {
        let appearance = light(transparency);
        for (host_role, colors) in [
            (
                spaceterm_ui::ControlHost::Window,
                &appearance.control_colors,
            ),
            (
                spaceterm_ui::ControlHost::TitleBar,
                &appearance.title_bar_controls.colors,
            ),
            (
                spaceterm_ui::ControlHost::Panel,
                &appearance.panel_controls.colors,
            ),
            (
                spaceterm_ui::ControlHost::Card,
                &appearance.card_controls.colors,
            ),
            (
                spaceterm_ui::ControlHost::Floating,
                &appearance.floating_control_colors,
            ),
        ] {
            let host = appearance.control_host_background(host_role);
            for (family, normal, hover) in [
                ("ordinary", colors.element_background, colors.element_hover),
                (
                    "ghost",
                    colors.ghost_element_background,
                    colors.ghost_element_hover,
                ),
            ] {
                let normal = normal.source_over(host);
                let hover = hover.source_over(host);
                assert!(
                    hover.contrast_ratio(normal) >= 1.10,
                    "{family} {host_role:?} hover at {transparency} disappears: {hover:?} vs {normal:?}"
                );
            }
        }
    }
}

#[test]
fn light_settings_uses_the_workspace_content_hierarchy() {
    use super::appearance::settings::{
        self,
        SettingsSurfaceRole::{Canvas, Card, Sidebar},
    };

    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_light(transparency);
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let content = active.pane_surface(resolved.terminal.colors.background);
        let (settings, _) = settings::prepare_variants(&resolved.chrome, active, inactive);
        let sidebar = settings.surface(Sidebar);
        let canvas = settings.surface(Canvas);
        let card = settings.surface(Card);

        assert_eq!(
            canvas.semantic, resolved.terminal.colors.background,
            "Settings and the built-in Terminal must share the content tone"
        );
        assert_eq!(
            canvas.paint, content,
            "Content surfaces must share the same transmission treatment"
        );
        assert!(
            sidebar.background.r < card.background.r && card.background.r < canvas.background.r,
            "Light hierarchy at {transparency}: muted navigation {sidebar:?}, grouped content {card:?}, light canvas {canvas:?}"
        );
        assert_eq!(
            settings.card_edge().a,
            0,
            "Normal Light groups separate by fill instead of a decorative outline"
        );
    }
}

#[test]
fn light_settings_preserves_an_explicit_group_surface_override() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.background.transparency = 0.0;
    let group = Color::rgb(0xe8eef4);
    preferences.chrome.overrides.insert(
        crate::appearance::builtin_light_chrome(),
        crate::appearance::ChromeColorOverrides {
            elevated_surface_background: Some(group),
            ..Default::default()
        },
    );
    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light),
            &AvailableFonts::default(),
        )
        .unwrap();
    let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
    let (settings, _) =
        super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
    assert_eq!(
        settings
            .surface(super::appearance::settings::SettingsSurfaceRole::Card)
            .paint,
        group,
        "A built-in scheme override must remain the authored group color, not a mixed tone"
    );
}

#[test]
fn light_settings_keeps_accessibility_group_boundaries() {
    for increase_contrast in [false, true] {
        let mut resolved = resolve_light(0.35);
        let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
        chrome.composition.capabilities.increase_contrast = increase_contrast;
        chrome.composition.capabilities.show_borders = !increase_contrast;
        let (active, inactive) = ChromeAppearance::prepare_variants(chrome);
        let (settings, _) = super::appearance::settings::prepare_variants(chrome, active, inactive);
        assert!(
            settings.card_edge().a > 0,
            "Accessibility must restore the group boundary"
        );
        assert!(
            settings.sidebar_edge().is_some(),
            "Accessibility must restore the navigation boundary"
        );
    }
}

#[test]
fn light_settings_control_hover_retains_a_visible_step_on_scoped_hosts() {
    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_light(transparency);
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings, _) =
            super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
        let appearance = &settings.chrome;
        for (role, colors) in [
            (
                spaceterm_ui::ControlHost::Window,
                &appearance.control_colors,
            ),
            (
                spaceterm_ui::ControlHost::Panel,
                &appearance.panel_controls.colors,
            ),
            (
                spaceterm_ui::ControlHost::Card,
                &appearance.card_controls.colors,
            ),
        ] {
            let host = appearance.control_host_background(role);
            for (normal, hover) in [
                (colors.element_background, colors.element_hover),
                (colors.ghost_element_background, colors.ghost_element_hover),
            ] {
                assert!(
                    hover
                        .source_over(host)
                        .contrast_ratio(normal.source_over(host))
                        >= 1.10,
                    "Settings {role:?} hover at {transparency} must remain visible"
                );
            }
        }
    }
}
