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

/// Hover is read over the shell the window renders, so that is where it has to hold its step.
///
/// The opaque reference the fill is solved from is not a surface anyone sees once the window
/// transmits, and no authored tone can hold a tenth of a step against it through the material.
/// Over a desktop the same ink reads plainly, and it grows rather than fades as the Setting
/// rises, because equal ink buys a wider ratio the darker its backing is.
#[test]
fn light_unselected_navigation_hover_remains_visible_through_materials() {
    let desktop = Color::rgb(0x808080);
    for transparency in [0.0, 0.35, 1.0] {
        let appearance = light(transparency);
        let shell = appearance
            .surface(
                crate::appearance::SurfaceRole::Sheet,
                appearance.colors.background,
            )
            .source_over(desktop);
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
            // The chip is solved against its semantic host and rendered over the shell.
            let hovered = chip.hover_fill.unwrap().source_over(shell);
            assert!(
                hovered.contrast_ratio(shell) >= 1.10,
                "{name} hover at {transparency} disappears: {hovered:?} over {shell:?}"
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

/// Settings shares the Workspace's content tone, and keeps the denser paint its groups need.
///
/// A Pane hosts text and nothing else, so it can transmit with the window. The Settings canvas
/// hosts inset groups whose own step is measured from it, and those groups would sink past the
/// navigation beside them if the canvas thinned with the Setting. The two content surfaces
/// therefore share the tone rather than the paint.
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
        assert!(
            canvas.paint.a >= content.a,
            "the Settings canvas must keep at least the Pane's ink at {transparency}: canvas={:?}, pane={content:?}",
            canvas.paint,
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

/// A chip is lifted, not outlined, and the Pane is the one surface that states a real boundary.
///
/// A Tab and a selected row sit on the strip behind them, so their rim only has to catch the
/// light an edge would: enough to lift the shape, not enough to draw a line around it. The Pane
/// separates the reading surface from the Chrome, which is a boundary, so it keeps the stronger
/// hairline. Dark states the lift most quietly, because light ink on a dark strip reads as a
/// frame long before the same step does on a bright one.
#[test]
fn every_chip_lifts_without_drawing_a_border() {
    use crate::appearance::Appearance;

    let desktop = Color::rgb(0x2b3a55);
    let mut loudest = std::collections::BTreeMap::new();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            let mut preferences = AppearancePreferences {
                mode,
                ..Default::default()
            };
            preferences.background.transparency = transparency;
            let resolved = SchemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable()
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .unwrap();
            let appearance = ChromeAppearance::prepare(&resolved.chrome);
            let panel = &appearance.panel_controls.reference;
            let unfocused = appearance.unfocused_selection_colors(spaceterm_ui::ControlHost::Panel);
            // How far a rim carries the surface it is painted on away from that surface.
            let lift = |rim: Color, fill: Color| rim.source_over(fill).contrast_ratio(fill) - 1.0;

            let pane_fill = appearance
                .pane_surface(resolved.terminal.colors.background)
                .source_over(appearance.colors.background.source_over(desktop));
            let pane_lift = lift(
                appearance.pane_rim_on(resolved.terminal.colors.background),
                pane_fill,
            );

            for (name, semantic_host, fill, rim) in [
                (
                    "Active Tab",
                    appearance.colors.title_bar_background,
                    resolved.chrome.colors.tab_active_background,
                    appearance.colors.tab_active_border,
                ),
                (
                    "focused sidebar row",
                    appearance.colors.panel_background,
                    panel.row_selected_background,
                    panel.row_selected_border,
                ),
                (
                    "unfocused sidebar row",
                    appearance.colors.panel_background,
                    unfocused.row_selected_background,
                    unfocused.row_selected_border,
                ),
            ] {
                let paint = super::selection_chip::ChipPaint {
                    fill: Some(fill),
                    rim: Some(rim),
                    hover_fill: None,
                    hover_rim: None,
                }
                .selected_on(&appearance, semantic_host);
                let rim = paint.rim.unwrap();
                assert!(
                    rim.a > 0,
                    "{mode:?} at {transparency}: the {name} lift is missing"
                );
                assert_eq!(
                    rim.r > 128,
                    resolved.chrome.appearance == Appearance::Dark,
                    "{mode:?} at {transparency}: the {name} rim uses the wrong ink: {rim:?}"
                );
                let painted = paint
                    .fill
                    .unwrap()
                    .source_over(semantic_host.source_over(desktop));
                let chip_lift = lift(rim, painted);
                assert!(
                    chip_lift >= 0.05,
                    "{mode:?} at {transparency}: the {name} lift vanishes at {chip_lift:.3}"
                );
                // A bright fill carries more edge before it reads as a line, so Light is allowed
                // the wider share of the Pane's boundary and Dark is held well under it.
                let ceiling = if resolved.chrome.appearance == Appearance::Dark {
                    0.5
                } else {
                    0.85
                };
                assert!(
                    chip_lift <= pane_lift * ceiling,
                    "{mode:?} at {transparency}: the {name} rim reads as a border at \
                     {chip_lift:.3} against the Pane boundary's {pane_lift:.3}"
                );
                let share = loudest.entry(mode as u8).or_insert(0.0_f64);
                *share = share.max(chip_lift / pane_lift);
            }
        }
    }
    assert!(
        loudest[&(AppearanceMode::Dark as u8)] < loudest[&(AppearanceMode::Light as u8)],
        "Dark must state the lift more quietly than Light: {loudest:?}"
    );
}

/// A chip's geometry is a question of density, never of Light or Dark.
#[test]
fn chip_geometry_matches_across_appearances() {
    use crate::appearance::AppearanceMode;

    let mut sizes = Vec::new();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let preferences = crate::appearance::AppearancePreferences {
            mode,
            ..Default::default()
        };
        let resolved = crate::appearance::SchemeCatalog::default()
            .resolve(
                crate::appearance::AppearanceGeneration::INITIAL,
                &preferences,
                crate::appearance::SystemAppearance::unavailable(),
                &crate::appearance::AvailableFonts::default(),
            )
            .unwrap();
        let appearance = ChromeAppearance::prepare(&resolved.chrome);
        sizes.push((
            appearance.spacing_scale,
            appearance.spacing(super::workspace_sidebar::SIDEBAR_ROW_SELECTION_INSET_Y),
            appearance
                .typography
                .style(crate::ui::chrome_typography::TextRole::Body)
                .line_height,
        ));
    }
    assert_eq!(
        sizes[0], sizes[1],
        "chip geometry must not follow appearance"
    );
}
