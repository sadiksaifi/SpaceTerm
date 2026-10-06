use std::collections::BTreeSet;

use super::*;

#[test]
fn floating_backdrop_alpha_limit_only_opens_over_an_effective_native_backdrop() {
    let mut preferences = AppearancePreferences::default();
    preferences.window.opacity = 0.0;

    let supported = ResolvedWindowComposition::resolve(
        &preferences.window,
        CompositionCapabilities::new(true, true),
        ChromeTone::Dark,
    );
    let unsupported = ResolvedWindowComposition::resolve(
        &preferences.window,
        CompositionCapabilities::new(false, true),
        ChromeTone::Dark,
    );
    let inaccessible = ResolvedWindowComposition::resolve(
        &preferences.window,
        CompositionCapabilities::new(true, false),
        ChromeTone::Dark,
    );

    assert!((supported.materials.floating_backdrop_alpha_limit() - 0.15).abs() < f32::EPSILON);
    assert_eq!(unsupported.materials.floating_backdrop_alpha_limit(), 1.0);
    assert_eq!(inaccessible.materials.floating_backdrop_alpha_limit(), 1.0);

    preferences.window.opacity = 1.0;
    let opaque = ResolvedWindowComposition::resolve(
        &preferences.window,
        CompositionCapabilities::new(true, true),
        ChromeTone::Dark,
    );
    assert_eq!(opaque.materials.floating_backdrop_alpha_limit(), 1.0);
}

#[test]
fn opacity_resolves_endpoints_in_both_modes_without_changing_theme_colors() {
    let catalog = ThemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        let opaque = resolve(&preferences);
        for opacity in [1.0, 0.85, 0.65, 0.0] {
            preferences.window.opacity = opacity;
            let resolved = catalog
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable()
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .unwrap();
            assert_eq!(resolved.chrome.colors, opaque.chrome.colors);
            assert_eq!(resolved.terminal.colors, opaque.terminal.colors);
            let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
            let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
            let pane = prepared.pane_surface(opaque.terminal.colors.background);
            let controls = &prepared.control_colors;
            assert_eq!(controls.text, prepared.colors.text);
            assert_eq!(
                controls.border,
                prepared
                    .materials
                    .edge(prepared.colors.background, prepared.colors.border)
            );
            let rim_band = if mode == AppearanceMode::Dark {
                1.25..=1.50
            } else {
                1.20..=1.30
            };
            let host = pane
                .source_over(prepared.control_host_background(spaceterm_ui::ControlHost::Window));
            let edge = prepared
                .pane_rim_on(resolved.terminal.colors.background)
                .source_over(host);
            assert!(rim_band.contains(&edge.contrast_ratio(host)));
            if opacity == 1.0 {
                assert_eq!(pane, opaque.terminal.colors.background);
                assert_eq!(sheet.a, 255);
                assert_eq!(controls.row_selected_background.a, 255);
                assert_eq!(controls.border, prepared.colors.border);
                assert_eq!(
                    resolved.chrome.composition.effective,
                    WindowBackgroundAppearance::Opaque
                );
            } else {
                assert_eq!(
                    resolved.chrome.composition.effective,
                    WindowBackgroundAppearance::Blurred
                );
                assert!(
                    controls.elevated_surface_background.a > sheet.a,
                    "{mode:?} at {opacity}: an elevated resting surface retains more color than the sheet"
                );
                if opacity == 0.0 {
                    // The sheet clears while elevated controls keep enough tint to retain shape.
                    assert_eq!(sheet.a, 0);
                    assert!(pane.a > 0 && pane.a < 255);
                    assert!(controls.row_selected_background.a > 0);
                    assert!(controls.elevated_surface_background.a >= 96);
                } else if mode == AppearanceMode::Light {
                    // Check transmission through two resting layers above the window sheet.
                    let retained = (1.0 - f64::from(pane.a) / 255.0)
                        * (1.0 - f64::from(controls.row_selected_background.a) / 255.0);
                    assert!(retained > 0.0, "Light surfaces must still transmit");
                }
            }
        }
    }
}

/// The Pane material of one appearance, prepared at one Opacity Setting.
fn prepared_appearance(mode: AppearanceMode, opacity: f32) -> ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode,
        ..Default::default()
    };
    preferences.window.opacity = opacity;
    ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable()
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .unwrap()
}

/// A Dark Pane sits one quiet step above the window root, measured against the opaque reference.
#[test]
fn dark_pane_rests_one_subtle_step_above_the_window_root() {
    for opacity in [1.0, 0.95, 0.85, 0.65, 0.3, 0.0] {
        let resolved = prepared_appearance(AppearanceMode::Dark, opacity);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let root = prepared.colors.background;
        let pane = prepared
            .pane_surface(resolved.terminal.colors.background)
            .source_over(root);

        assert!(
            [(pane.r, root.r), (pane.g, root.g), (pane.b, root.b)]
                .into_iter()
                .all(|(pane, root)| pane > root),
            "a Dark Pane stays lighter than the window root at opacity {opacity}: pane={pane:?}",
        );
        let step = root.contrast_ratio(pane);
        assert!(
            (1.015..=1.07).contains(&step),
            "a Dark Pane stays within one subtle step of the window root at opacity {opacity}: step={step}, pane={pane:?}",
        );
    }
}

/// A Dark Pane transmits with the window and keeps only the ink its step below the root costs.
#[test]
fn dark_pane_transmits_what_the_opacity_setting_asks() {
    let settings = [0.0_f32, 0.3, 0.65, 0.85, 1.0];
    let mut previous: Option<f64> = None;
    for opacity in settings {
        let resolved = prepared_appearance(AppearanceMode::Dark, opacity);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
        let pane = prepared.pane_surface(resolved.terminal.colors.background);
        let transmitted = |fill: Color| 1.0 - f64::from(fill.a) / 255.0;
        let chrome = transmitted(sheet);
        let terminal = chrome * transmitted(pane);

        if let Some(previous) = previous {
            assert!(
                terminal < previous,
                "a Dark Pane admits less desktop as opacity rises: {terminal} at opacity {opacity} after {previous}",
            );
        }
        previous = Some(terminal);
        assert!(
            terminal >= chrome * 0.8,
            "a Dark Pane admits nearly what the chrome admits at opacity {opacity}: terminal={terminal}, chrome={chrome}",
        );
        assert!(
            terminal < chrome || opacity == 1.0,
            "a Dark Pane stays denser than the chrome at opacity {opacity}",
        );
    }
}

/// An authored Terminal background that states a color keeps it; a neutral one joins the ladder.
#[test]
fn dark_pane_keeps_a_stated_terminal_background_apart_from_the_neutral_ladder() {
    let resolved = prepared_appearance(AppearanceMode::Dark, 0.65);
    let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
    let neutral = prepared.pane_surface(resolved.terminal.colors.background);
    let stated = prepared.pane_surface(Color::rgb(0x002b36));

    assert!(
        stated.a > neutral.a * 2,
        "a stated Terminal background spends more ink than a neutral rung: stated={stated:?}, neutral={neutral:?}",
    );
    let rendered = stated.source_over(prepared.colors.background);
    assert!(
        rendered.b > rendered.r,
        "a stated Terminal background keeps its hue: {rendered:?}",
    );
}

#[test]
fn light_material_keeps_elevation_and_selection_visible_over_the_sheet() {
    let preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    let resolved = ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable()
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .unwrap();
    let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
    for backdrop in [Color::rgb(0xb0b0b0), Color::rgb(0xf0f0f0)] {
        let sheet = prepared
            .surface(SurfaceRole::Sheet, prepared.colors.background)
            .source_over(backdrop);
        for target in [
            resolved.terminal.colors.background,
            prepared.colors.elevated_surface_background,
        ] {
            let overlay = prepared.surface(SurfaceRole::Surface, target);
            let raised = overlay.source_over(sheet);
            assert!(raised.r > sheet.r && raised.g > sheet.g && raised.b > sheet.b);
            assert!(
                [
                    raised.r.saturating_sub(sheet.r),
                    raised.g.saturating_sub(sheet.g),
                    raised.b.saturating_sub(sheet.b)
                ]
                .into_iter()
                .all(|delta| delta >= 3),
                "Light elevation should remain visible: {raised:?} over {sheet:?}"
            );
            assert!(
                overlay.a < 128,
                "a resting surface must continue to transmit most of its backing"
            );
        }
        let selection_overlay = prepared.materials.paint(
            SurfaceRole::Surface,
            prepared.colors.background,
            prepared.colors.selection_background,
        );
        let selection = selection_overlay.source_over(sheet);
        assert!(
            selection.r > sheet.r && selection.g > sheet.g && selection.b > sheet.b,
            "Light selection should lift off its sheet host: {selection:?} against {sheet:?}"
        );
        assert!(
            [
                selection.r.abs_diff(sheet.r),
                selection.g.abs_diff(sheet.g),
                selection.b.abs_diff(sheet.b),
            ]
            .into_iter()
            .all(|delta| delta >= 5),
            "Light selection should remain visible against its sheet host: {selection:?} against \
             {sheet:?}"
        );
        assert!(
            selection_overlay.a < 128,
            "a selection must continue to transmit most of its sheet host"
        );
        let shell = prepared
            .surface(SurfaceRole::Base, prepared.colors.panel_background)
            .source_over(sheet);
        let selected_overlay = prepared.materials.paint(
            SurfaceRole::Surface,
            prepared.colors.panel_background,
            prepared.colors.row_selected_background,
        );
        let selected = selected_overlay.source_over(shell);
        assert!(
            selected.r > shell.r && selected.g > shell.g && selected.b > shell.b,
            "Light selection should lift off its shell host: {selected:?} against {shell:?}"
        );
        assert!(
            [
                selected.r.abs_diff(shell.r),
                selected.g.abs_diff(shell.g),
                selected.b.abs_diff(shell.b),
            ]
            .into_iter()
            .all(|delta| delta >= 5),
            "Light selection should remain visible against its shell host: {selected:?} against \
             {shell:?}"
        );
        assert!(
            selected_overlay.a < 128,
            "a selected row must continue to transmit most of its shell host"
        );
    }
}

#[test]
fn light_navigation_selections_share_one_contrast_direction_across_material_settings() {
    let catalog = ThemeCatalog::default();
    let desktop = Color::rgb(0x808080);
    let weight = |color: Color| i32::from(color.r) + i32::from(color.g) + i32::from(color.b);

    for opacity in [1.0, 0.65, 0.0] {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..Default::default()
        };
        preferences.window.opacity = opacity;
        let resolved = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable()
                    .with_composition(CompositionCapabilities::new(true, true)),
                &AvailableFonts::default(),
            )
            .unwrap();
        let appearance = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let colors = &appearance.colors;
        let sheet = appearance
            .surface(SurfaceRole::Sheet, colors.background)
            .source_over(desktop);
        let tab_shell = appearance
            .surface(SurfaceRole::Base, colors.title_bar_background)
            .source_over(sheet);
        let sidebar_shell = appearance
            .surface(SurfaceRole::Base, colors.panel_background)
            .source_over(sheet);
        assert_eq!(
            tab_shell, sidebar_shell,
            "Light navigation hosts should share one shell at {opacity}"
        );
        assert_eq!(
            tab_shell, sheet,
            "Light navigation shell should match the root at {opacity}"
        );

        let active_tab = appearance
            .materials
            .paint(
                SurfaceRole::Surface,
                colors.title_bar_background,
                colors.tab_active_background,
            )
            .source_over(tab_shell);
        let selected_sidebar = appearance
            .materials
            .paint(
                SurfaceRole::Surface,
                colors.panel_background,
                colors.navigation_selected_background,
            )
            .source_over(sidebar_shell);
        let tab_direction = (weight(active_tab) - weight(tab_shell)).signum();
        let sidebar_direction = (weight(selected_sidebar) - weight(sidebar_shell)).signum();

        assert_ne!(
            tab_direction, 0,
            "Active Tab should remain distinct from its Light shell at {opacity}"
        );
        assert_ne!(
            sidebar_direction, 0,
            "selected sidebar row should remain distinct from its Light shell at {opacity}"
        );
        assert_eq!(
            tab_direction, sidebar_direction,
            "Active Tab and selected sidebar row should move in the same contrast direction from \
             their Light shell at {opacity}: tab={active_tab:?}, sidebar={selected_sidebar:?}, \
             shell={tab_shell:?}"
        );
        assert_eq!(
            (colors.tab_active_border.a, colors.row_selected_border.a),
            (4, 4),
            "Light navigation should retain its authored chip lift at {opacity}"
        );
    }
}

/// A Light Pane sits one quiet step above the chrome: the authored 1.21 while opaque, narrowing
/// to the ladder ceiling as glass engages.
#[test]
fn light_pane_rests_one_subtle_step_above_the_window_root() {
    for opacity in [1.0, 0.95, 0.85, 0.65, 0.3, 0.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, opacity);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let root = prepared.colors.background;
        let pane = prepared
            .pane_surface(resolved.terminal.colors.background)
            .source_over(root);

        assert!(
            [(pane.r, root.r), (pane.g, root.g), (pane.b, root.b)]
                .into_iter()
                .all(|(pane, root)| pane > root),
            "a Light Pane stays brighter than the window root at opacity {opacity}: pane={pane:?}",
        );
        let step = pane.contrast_ratio(root);
        assert!(
            (1.035..=1.24).contains(&step),
            "a Light Pane stays within one subtle step of the window root at opacity {opacity}: step={step}, pane={pane:?}",
        );
    }
}

/// A Light Pane transmits with the window and keeps only the overlay its step above the root
/// costs, which is wider than a Dark Pane's.
#[test]
fn light_pane_transmits_what_the_opacity_setting_asks() {
    let settings = [0.0_f32, 0.3, 0.65, 0.85, 1.0];
    let mut previous: Option<f64> = None;
    for opacity in settings {
        let resolved = prepared_appearance(AppearanceMode::Light, opacity);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
        let pane = prepared.pane_surface(resolved.terminal.colors.background);
        let transmitted = |fill: Color| 1.0 - f64::from(fill.a) / 255.0;
        let chrome = transmitted(sheet);
        let terminal = chrome * transmitted(pane);

        if let Some(previous) = previous {
            assert!(
                terminal < previous,
                "a Light Pane admits less desktop as opacity rises: {terminal} at opacity {opacity} after {previous}",
            );
        }
        previous = Some(terminal);
        // Near-white ink over a near-white root costs more coverage, so this bound is below Dark's.
        assert!(
            terminal >= chrome * 0.6,
            "a Light Pane admits nearly what the chrome admits at opacity {opacity}: terminal={terminal}, chrome={chrome}",
        );
        assert!(
            terminal < chrome || opacity == 1.0,
            "a Light Pane stays denser than the chrome at opacity {opacity}",
        );
    }
}

/// A Light selected chip spends one overlay for its step and transmits the rest, so its contrast
/// stays within a band as opacity falls.
#[test]
fn light_selected_navigation_keeps_one_step_across_the_setting() {
    use spaceterm_ui::ControlHost;

    let desktop = Color::rgb(0x2b3a55);
    for opacity in [1.0, 0.85, 0.65, 0.3, 0.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, opacity);
        let appearance = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let shell = appearance
            .surface(SurfaceRole::Sheet, appearance.colors.background)
            .source_over(desktop);
        let panel = appearance.host_colors(ControlHost::Panel);
        let selection = appearance.unfocused_selection_colors(ControlHost::Panel);
        for (name, host, fill) in [
            (
                "tab",
                appearance
                    .host_colors(ControlHost::TitleBar)
                    .title_bar_background,
                appearance
                    .host_colors(ControlHost::TitleBar)
                    .tab_active_background,
            ),
            (
                "sidebar row",
                panel.row_background,
                selection.row_selected_background,
            ),
        ] {
            let chip = appearance.selection_surface(host, fill).source_over(shell);
            let step = chip.contrast_ratio(shell);
            assert!(
                (1.15..=2.35).contains(&step),
                "a Light {name} holds one step from its shell at opacity {opacity}: step={step}, chip={chip:?} over {shell:?}",
            );
            assert!(
                chip.r > shell.r,
                "a Light {name} lifts from its shell at opacity {opacity}",
            );
            let ink = appearance.selection_surface(host, fill).a;
            assert_eq!(
                ink == 255,
                opacity == 1.0,
                "a Light {name} transmits at opacity {opacity}: ink={ink}",
            );
        }
    }
}

/// The Light Terminal backing is the Pane's own color under the window material, with no
/// elevation rung of its own, and window activation does not change it.
#[test]
fn light_terminal_backing_applies_opacity_without_an_elevation_tint() {
    let mut previous: Option<u8> = None;
    for opacity in [1.0, 0.95, 0.85, 0.65, 0.3, 0.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, opacity);
        let background = resolved.terminal.colors.background;
        let (active, inactive) =
            crate::ui::appearance::ChromeAppearance::prepare_variants(&resolved.chrome);
        let alpha = active.pane_surface(background).a;
        for prepared in [active, inactive] {
            let pane = prepared.pane_surface(background);
            assert_eq!(
                pane,
                prepared.surface(SurfaceRole::Surface, background),
                "Light Terminal at opacity {opacity}, active={}",
                prepared.active,
            );
            assert!(
                pane.r == pane.g && pane.g == pane.b,
                "a neutral Light Terminal background must not acquire a tint: {pane:?}",
            );
        }
        if let Some(previous) = previous {
            assert!(
                alpha < previous,
                "the Light Terminal backing thins as opacity falls: {alpha} at opacity {opacity} after {previous}",
            );
        }
        previous = Some(alpha);
    }
}

#[test]
fn light_terminal_backing_uses_custom_background_as_its_material_color() {
    let resolved = resolve(&AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    });
    let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
    for background in [Color::rgb(0x18324c), Color::rgba(0xe8d9b780)] {
        assert_eq!(
            prepared.pane_surface(background),
            prepared.surface(SurfaceRole::Surface, background)
        );
    }
}

#[test]
fn selected_surfaces_follow_opacity_in_both_appearances() {
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        for opacity in [1.0, 0.65, 0.0] {
            let mut preferences = AppearancePreferences {
                mode,
                ..Default::default()
            };
            preferences.window.opacity = opacity;
            let resolved = ThemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable()
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .unwrap();
            let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
            let selected = prepared.selection_surface(
                prepared.colors.panel_background,
                prepared.colors.row_selected_background,
            );
            if opacity == 1.0 {
                assert_eq!(selected, prepared.colors.row_selected_background);
            } else {
                assert!(
                    selected.a > 0 && selected.a < 255,
                    "{mode:?} selected surface must transmit its backdrop at {opacity}: {selected:?}"
                );
                // Against the rendered shell, the promised bound is the ink spent, not a ratio.
                let host = prepared.colors.panel_background;
                assert!(
                    selected.source_over(host).contrast_ratio(host) > 1.0,
                    "{mode:?} selection must remain visible while transmitting its host"
                );
                assert!(
                    selected.source_over(host).r > host.r,
                    "built-in raised selections must not turn into dark recesses"
                );
            }
        }
    }
}

/// Row hover and selection must remain distinct on every host once their fills are translucent.
#[test]
fn surface_ladder_holds_its_order_as_opacity_decreases() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let reference = builtin_chrome_base(appearance).opaque_presentation();
        for opacity in [0.95, 0.85, 0.65, 0.3, 0.0] {
            let mut preferences = AppearancePreferences::default();
            preferences.window.opacity = opacity;
            let materials = ResolvedWindowComposition::resolve(
                &preferences.window,
                CompositionCapabilities::new(true, true),
                ChromeTone::of(reference.background),
            )
            .materials;
            let pane = materials.paint(
                SurfaceRole::Surface,
                reference.background,
                reference.elevated_surface_background,
            );
            assert!(
                pane.a > 0,
                "{appearance:?} at {opacity}: Pane material disappeared"
            );
            for (host_name, host) in [
                ("shell", reference.panel_background),
                ("raised surface", reference.elevated_surface_background),
            ] {
                let hover =
                    materials.paint(SurfaceRole::Surface, host, reference.row_hover_background);
                let selected = materials.paint(
                    SurfaceRole::Surface,
                    host,
                    reference.row_selected_background,
                );
                if host == reference.row_selected_background {
                    assert_eq!(selected.a, 0, "equal host and fill need no overlay");
                    continue;
                }
                assert!(
                    hover.a > 0 && selected.a > hover.a,
                    "{appearance:?} at {opacity}: row ladder collapsed on the \
                     {host_name}: hover={} selection={}",
                    hover.a,
                    selected.a
                );
            }
        }
    }
}

/// Light's hierarchy is checked as rendered over a desktop: the navigation shell matches the
/// root, while raised surfaces and navigation selections lift across the opacity range.
#[test]
fn light_surfaces_separate_over_the_desktop_at_every_setting() {
    let reference = builtin_chrome_base(Appearance::Light).opaque_presentation();
    let desktop = Color::rgb(0x808080);
    for (opacity, minimum) in [(0.85, 11_u8), (0.65, 10), (0.3, 5), (0.0, 2)] {
        let mut preferences = AppearancePreferences::default();
        preferences.window.opacity = opacity;
        let materials = ResolvedWindowComposition::resolve(
            &preferences.window,
            CompositionCapabilities::new(true, true),
            ChromeTone::Bright,
        )
        .materials;
        let sheet = materials
            .paint(
                SurfaceRole::Sheet,
                reference.background,
                reference.background,
            )
            .source_over(desktop);
        let rendered = |role, target| {
            materials
                .paint(role, reference.background, target)
                .source_over(sheet)
        };
        let shell = rendered(SurfaceRole::Base, reference.panel_background);
        let selected_navigation = materials
            .paint(
                SurfaceRole::Surface,
                reference.panel_background,
                reference.navigation_selected_background,
            )
            .source_over(shell);
        let pane = rendered(
            SurfaceRole::Surface,
            builtin::terminal_base(Appearance::Light).background,
        );
        for (name, surface, host) in [
            ("Pane against the sheet", pane, sheet),
            (
                "selected navigation row against the shell",
                selected_navigation,
                shell,
            ),
        ] {
            let step = surface.g.abs_diff(host.g);
            assert!(
                step >= minimum.min(3),
                "{name} at {opacity}: {step} levels of separation"
            );
        }
        assert!(pane.g > sheet.g, "a Pane should lift from the light sheet");
        assert_eq!(
            shell, sheet,
            "the Light navigation shell should match the root"
        );
        assert!(
            selected_navigation.g > shell.g,
            "a selected navigation row should lift from the Light shell"
        );
        assert!(
            pane.g.saturating_sub(sheet.g) >= minimum,
            "a Pane at {opacity} must stay clearly above the sheet: {pane:?} over {sheet:?}"
        );
    }
}

#[test]
fn opacity_rejects_invalid_numbers() {
    let mut preferences = AppearancePreferences::default();
    for invalid in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        preferences.window.opacity = invalid;
        assert!(preferences.validate().is_err());
    }
}

fn resolve(preferences: &AppearancePreferences) -> ResolvedAppearance {
    ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap()
}

#[test]
fn builtin_typography_resolves_product_sizes_and_weights() {
    let resolved = resolve(&AppearancePreferences::default());
    let typography = &resolved.chrome.typography;
    assert_eq!(typography.body.size, 13.0);
    assert_eq!(typography.body.weight, 400);
    assert_eq!(typography.caption.size, 12.65);
    assert_eq!(typography.caption.weight, 400);
    assert_eq!(typography.navigation.weight, 600);
    assert_eq!(typography.heading.weight, 600);
    assert_eq!(
        typography.body.primary_family,
        AvailableFonts::default().system_ui.family
    );
}

#[test]
fn defaults_select_spaceterm_owned_themes_in_both_appearances() {
    let catalog = ThemeCatalog::default();
    for (mode, suffix) in [
        (AppearanceMode::Light, "light"),
        (AppearanceMode::Dark, "dark"),
    ] {
        let preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        let resolved = resolve(&preferences);
        assert_eq!(
            resolved.terminal.effective_theme.as_str(),
            format!("builtin.spaceterm.{suffix}")
        );
        let terminal = catalog.get(&resolved.terminal.effective_theme).unwrap();
        assert_eq!(
            terminal.metadata.author.as_deref(),
            Some("SpaceTerm contributors")
        );
    }
}

#[test]
fn built_in_resting_surfaces_do_not_introduce_a_color_cast_at_any_opacity() {
    let catalog = ThemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        for step in 0..=100 {
            preferences.window.opacity = step as f32 / 100.0;
            let resolved = catalog
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::unavailable()
                        .with_composition(CompositionCapabilities::new(true, true)),
                    &AvailableFonts::default(),
                )
                .unwrap();
            let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
            let colors = &prepared.control_colors;
            let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
            let pane = prepared.pane_surface(resolved.terminal.colors.background);
            for (name, color) in [
                ("sheet", sheet),
                ("pane", pane),
                (
                    "pane rim",
                    prepared.pane_rim_on(resolved.terminal.colors.background),
                ),
                ("panel", colors.panel_background),
                ("popup", colors.elevated_surface_background),
                ("title bar", colors.title_bar_background),
                ("active tab", colors.tab_active_background),
                ("selected row", colors.row_selected_background),
                ("hovered row", colors.row_hover_background),
                ("selected row hover", colors.row_selected_hover_background),
                ("selected control", colors.selection_background),
                ("selected control hover", colors.selection_hover_background),
                ("control", colors.element_background),
                ("control hover", colors.element_hover),
                ("input", colors.input_background),
                ("disabled input", colors.input_disabled_background),
                ("preview", colors.preview_background),
                ("badge", colors.badge_background),
                ("border", colors.border),
                ("shadow", colors.shadow),
                ("scrim", colors.modal_scrim),
            ] {
                assert_eq!(color.r, color.g, "{mode:?} at {step}: {name}");
                assert_eq!(color.g, color.b, "{mode:?} at {step}: {name}");
            }
            // Neutral desktop light can change brightness, but must not acquire a theme hue.
            for desktop in [0x000000, 0x808080, 0xffffff].map(Color::rgb) {
                let rendered = pane.source_over(sheet.source_over(desktop));
                assert_eq!(rendered.r, rendered.g);
                assert_eq!(rendered.g, rendered.b);
            }
        }
    }
}

#[test]
fn color_and_typography_dimensions_resolve_independently() {
    let defaults = AppearancePreferences::default();
    let baseline = resolve(&defaults);

    let mut terminal_colors = defaults.clone();
    terminal_colors.terminal.overrides.insert(
        builtin_fallback_theme(Appearance::Dark),
        TerminalColorOverrides {
            foreground: Some(Color::rgb(0xaabbcc)),
            ..Default::default()
        },
    );
    let changed = resolve(&terminal_colors);
    assert_ne!(changed.terminal.colors, baseline.terminal.colors);
    assert_eq!(changed.terminal.typography, baseline.terminal.typography);
    assert_eq!(changed.chrome, baseline.chrome);

    let mut terminal_font = defaults;
    terminal_font.terminal.typography.base_size = 20.0;
    let changed = resolve(&terminal_font);
    assert_ne!(changed.terminal.typography, baseline.terminal.typography);
    assert_eq!(changed.terminal.colors, baseline.terminal.colors);
    assert_eq!(changed.chrome, baseline.chrome);
}

#[test]
fn one_mode_selects_the_matching_slot_for_both_domains() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let preferences = AppearancePreferences {
            mode: appearance.into(),
            ..Default::default()
        };
        let resolved = resolve(&preferences);
        assert_eq!(resolved.chrome.appearance, appearance);
        assert_eq!(resolved.terminal.appearance, appearance);
        assert_eq!(
            resolved.terminal.requested_theme,
            *preferences.terminal.themes.get(appearance)
        );
    }

    let preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    let catalog = ThemeCatalog::default();
    let light = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light),
            &AvailableFonts::default(),
        )
        .unwrap();
    let dark = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Dark),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(light.chrome.appearance, Appearance::Light);
    assert_eq!(light.terminal.appearance, Appearance::Light);
    assert_eq!(dark.chrome.appearance, Appearance::Dark);
    assert_eq!(dark.terminal.appearance, Appearance::Dark);
}

#[test]
fn missing_system_appearance_is_diagnostic_only_for_auto() {
    let catalog = ThemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        let resolved = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        assert!(
            !resolved
                .diagnostics
                .contains(&AppearanceDiagnostic::SystemAppearanceUnavailable)
        );
    }

    let preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    let resolved = resolve(&preferences);
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::SystemAppearanceUnavailable)
    );
    assert_eq!(resolved.chrome.appearance, Appearance::Dark);
    assert_eq!(resolved.terminal.appearance, Appearance::Dark);
}

#[test]
fn unavailable_resources_preserve_requests_and_report_effective_fallbacks() {
    let missing = ThemeId::new("missing.terminal").unwrap();
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.terminal.themes.light = missing.clone();
    preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: String::from("Unavailable Mono"),
    };
    let resolved = resolve(&preferences);
    assert_eq!(resolved.terminal.requested_theme, missing);
    assert_eq!(
        resolved.terminal.effective_theme.as_str(),
        "builtin.spaceterm.light"
    );
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::TerminalThemeUnavailable {
                appearance: Appearance::Light
            })
    );
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::TerminalFontUnavailable)
    );
}

#[test]
fn terminal_emoji_fallback_precedes_every_text_fallback() {
    let resolved = resolve(&AppearancePreferences::default());
    for descriptor in [
        &resolved.terminal.typography.regular,
        &resolved.terminal.typography.bold,
        &resolved.terminal.typography.italic,
        &resolved.terminal.typography.bold_italic,
    ] {
        assert_eq!(
            descriptor.fallback_families.first().map(String::as_str),
            Some("emoji")
        );
        assert_eq!(
            descriptor
                .fallback_families
                .iter()
                .filter(|family| family.as_str() == "emoji")
                .count(),
            1
        );
    }
}

#[test]
fn proportional_terminal_font_request_falls_back_to_monospace_without_reordering_emoji() {
    let mut preferences = AppearancePreferences::default();
    preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: String::from("Proportional Test"),
    };
    let fonts = AvailableFonts {
        installed: vec![AvailableFont {
            family: String::from("Proportional Test"),
            class: FontClass::Proportional,
            resolution_identity: String::from("proportional-test"),
        }],
        ..AvailableFonts::default()
    };
    let resolved = ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &fonts,
        )
        .unwrap();

    assert_eq!(
        resolved.terminal.typography.regular.primary_family,
        "monospace"
    );
    assert_eq!(
        resolved
            .terminal
            .typography
            .regular
            .fallback_families
            .first()
            .map(String::as_str),
        Some("emoji")
    );
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::TerminalFontNotMonospace)
    );
}

/// Resetting a Terminal slot preserves Appearance Mode and the other slot.
#[test]
fn resetting_a_theme_slot_keeps_the_mode_and_other_slots() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    preferences.terminal.themes.light = ThemeId::new("missing.changed.terminal").unwrap();
    let dark = preferences.terminal.themes.dark.clone();

    preferences.reset(ResetTarget::TerminalTheme(Appearance::Light));

    assert_eq!(preferences.mode, AppearanceMode::Auto);
    assert_eq!(
        preferences.terminal.themes.light,
        AppearancePreferences::default().terminal.themes.light
    );
    assert_eq!(preferences.terminal.themes.dark, dark);
}

#[test]
fn resetting_the_mode_preserves_every_theme_slot() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    let terminal = preferences.terminal.themes.clone();

    preferences.reset(ResetTarget::AppearanceMode);

    assert_eq!(preferences.mode, AppearanceMode::Dark);
    assert_eq!(preferences.terminal.themes, terminal);
}

#[test]
fn successful_catalog_install_advances_revision_and_rejects_stale_revision() {
    let theme = TerminalTheme {
        id: ThemeId::new("custom.blue").unwrap(),
        name: String::from("Blue"),
        appearance: Appearance::Dark,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides::default(),
    };
    let mut catalog = ThemeCatalog::default();
    assert_eq!(
        catalog
            .install_batch(std::slice::from_ref(&theme), 0, &BTreeSet::new())
            .unwrap(),
        vec![theme.id.clone()]
    );
    assert_eq!(catalog.revision(), 1);
    assert!(matches!(
        catalog.install_batch(&[theme], 0, &BTreeSet::new()),
        Err(CatalogError::RevisionConflict)
    ));
}

#[test]
fn color_encoding_accepts_short_forms_and_exports_long_rgba() {
    let color: Color = serde_json::from_str("\"#abc\"").unwrap();
    assert_eq!(color, Color::rgb(0xaabbcc));
    assert_eq!(serde_json::to_string(&color).unwrap(), "\"#aabbccff\"");
}

#[test]
fn bundled_default_preserves_a_named_system_font_choice() {
    let fonts = AvailableFonts {
        installed: vec![
            AvailableFont {
                family: "SpaceTerm Default".into(),
                class: FontClass::Monospace,
                resolution_identity: "bundled".into(),
            },
            AvailableFont {
                family: "JetBrainsMono Nerd Font".into(),
                class: FontClass::Monospace,
                resolution_identity: "system-installed".into(),
            },
        ],
        ..AvailableFonts::default()
    };
    let catalog = ThemeCatalog::default();
    let mut preferences = AppearancePreferences::default();
    let resolved = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &fonts,
        )
        .unwrap();
    assert_eq!(
        resolved.terminal.typography.regular.primary_family,
        "SpaceTerm Default"
    );
    preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: "JetBrainsMono Nerd Font".into(),
    };
    let resolved = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &fonts,
        )
        .unwrap();
    assert_eq!(
        resolved.terminal.typography.regular.primary_family,
        "JetBrainsMono Nerd Font"
    );
    assert_eq!(
        resolved.terminal.typography.regular.resolution_identity,
        "system-installed"
    );
}
