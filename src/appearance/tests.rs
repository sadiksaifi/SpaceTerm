use std::collections::BTreeSet;

use super::*;

#[test]
fn floating_backdrop_alpha_limit_only_opens_over_an_effective_native_backdrop() {
    let mut preferences = AppearancePreferences::default();
    preferences.background.transparency = 1.0;

    let supported = ResolvedWindowComposition::resolve(
        &preferences.background,
        CompositionCapabilities::new(true, true),
    );
    let unsupported = ResolvedWindowComposition::resolve(
        &preferences.background,
        CompositionCapabilities::new(false, true),
    );
    let inaccessible = ResolvedWindowComposition::resolve(
        &preferences.background,
        CompositionCapabilities::new(true, false),
    );

    assert!((supported.materials.floating_backdrop_alpha_limit() - 0.15).abs() < f32::EPSILON);
    assert_eq!(unsupported.materials.floating_backdrop_alpha_limit(), 1.0);
    assert_eq!(inaccessible.materials.floating_backdrop_alpha_limit(), 1.0);

    preferences.background.transparency = 0.0;
    let opaque = ResolvedWindowComposition::resolve(
        &preferences.background,
        CompositionCapabilities::new(true, true),
    );
    assert_eq!(opaque.materials.floating_backdrop_alpha_limit(), 1.0);
}

#[test]
fn transparency_resolves_endpoints_in_both_modes_without_changing_scheme_colors() {
    let catalog = SchemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        let opaque = resolve(&preferences);
        for transparency in [0.0, 0.15, 0.35, 1.0] {
            preferences.background.transparency = transparency;
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
            assert_eq!(
                prepared.pane_rim(),
                prepared.materials.edge(
                    prepared.colors.panel_background,
                    prepared.colors.tab_separator,
                )
            );
            if transparency == 0.0 {
                assert_eq!(pane, opaque.terminal.colors.background);
                assert_eq!(sheet.a, 255);
                assert_eq!(controls.row_selected_background.a, 255);
                assert_eq!(controls.border, prepared.colors.border);
                assert_eq!(prepared.pane_rim(), prepared.colors.tab_separator);
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
                    "{mode:?} at {transparency}: an elevated resting surface retains more color than the sheet"
                );
                if transparency == 1.0 {
                    // The sheet clears while Panes and controls keep enough tint to retain shape.
                    assert_eq!(sheet.a, 0);
                    assert!(pane.a > 0 && pane.a < 192);
                    assert!(controls.row_selected_background.a > 0);
                    assert!(controls.elevated_surface_background.a >= 96);
                } else {
                    // Two resting layers must retain a substantial share of the sheet's backdrop transmission.
                    // Previously, each added a full tint and almost erased it.
                    let retained = (1.0 - f64::from(pane.a) / 255.0)
                        * (1.0 - f64::from(controls.row_selected_background.a) / 255.0);
                    let repeated_full_tints = f64::from(transparency).powi(2);
                    assert!(
                        retained > repeated_full_tints * 3.0,
                        "{mode:?}: resting layers retain {retained}"
                    );
                }
            }
        }
    }
}

#[test]
fn light_material_keeps_elevation_and_selection_visible_over_the_sheet() {
    let preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    let resolved = SchemeCatalog::default()
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
    let catalog = SchemeCatalog::default();
    let desktop = Color::rgb(0x808080);
    let weight = |color: Color| i32::from(color.r) + i32::from(color.g) + i32::from(color.b);

    for transparency in [0.0, 0.35, 1.0] {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..Default::default()
        };
        preferences.background.transparency = transparency;
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
            "Light navigation hosts should share one shell at {transparency}"
        );
        assert_eq!(
            tab_shell, sheet,
            "Light navigation shell should match the root at {transparency}"
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
            "Active Tab should remain distinct from its Light shell at {transparency}"
        );
        assert_ne!(
            sidebar_direction, 0,
            "selected sidebar row should remain distinct from its Light shell at {transparency}"
        );
        assert_eq!(
            tab_direction, sidebar_direction,
            "Active Tab and selected sidebar row should move in the same contrast direction from \
             their Light shell at {transparency}: tab={active_tab:?}, sidebar={selected_sidebar:?}, \
             shell={tab_shell:?}"
        );
        assert_eq!(
            (colors.tab_active_border.a, colors.row_selected_border.a),
            (0, 0),
            "navigation selection should not require decorative borders at {transparency}"
        );
    }
}

#[test]
fn dark_terminal_backing_improves_text_contrast_and_reduces_desktop_variation() {
    let catalog = SchemeCatalog::default();
    for transparency in [0.15, 0.35, 0.7, 1.0] {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Dark,
            ..Default::default()
        };
        preferences.background.transparency = transparency;
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
        let terminal = &resolved.terminal.colors;
        let pane = prepared.pane_surface(terminal.background);
        let lift_only = prepared.surface(
            SurfaceRole::Surface,
            terminal.background.mix(
                prepared.colors.elevated_surface_background,
                f64::from(prepared.materials.elevation(prepared.colors.background)),
            ),
        );
        let render = |fill: Color, desktop: Color| {
            fill.source_over(
                prepared
                    .surface(SurfaceRole::Sheet, prepared.colors.background)
                    .source_over(desktop),
            )
        };
        for desktop in [0x808080, 0x906060, 0x608090].map(Color::rgb) {
            let protected = render(pane, desktop);
            let unprotected = render(lift_only, desktop);
            for text in [terminal.foreground, terminal.normal[2], terminal.normal[4]] {
                assert!(
                    text.contrast_ratio(protected) > text.contrast_ratio(unprotected),
                    "Dark at {transparency}: backing must improve text contrast"
                );
            }
        }
        let dark = render(pane, Color::rgb(0x303030));
        let bright = render(pane, Color::rgb(0x909090));
        let previous_dark = render(lift_only, Color::rgb(0x303030));
        let previous_bright = render(lift_only, Color::rgb(0x909090));
        for (new, previous) in [
            (bright.r - dark.r, previous_bright.r - previous_dark.r),
            (bright.g - dark.g, previous_bright.g - previous_dark.g),
            (bright.b - dark.b, previous_bright.b - previous_dark.b),
        ] {
            assert!(new > 0, "the Pane should still admit the desktop");
            assert!(
                f32::from(new) <= f32::from(previous) * 0.6 + 1.0,
                "the desktop should influence text backing substantially less"
            );
        }
    }
}

#[test]
fn light_terminal_backing_applies_transparency_without_an_elevation_tint() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    for transparency in [0.0, 0.05, 0.15, 0.35, 0.7, 1.0] {
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
        let background = resolved.terminal.colors.background;
        let (active, inactive) =
            crate::ui::appearance::ChromeAppearance::prepare_variants(&resolved.chrome);
        for prepared in [active, inactive] {
            assert_eq!(
                prepared.pane_surface(background),
                prepared.surface(SurfaceRole::Surface, background),
                "Light Terminal at transparency {transparency}, active={}",
                prepared.active,
            );
        }
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
fn selected_surfaces_follow_transparency_in_both_appearances() {
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
            let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
            let selected = prepared.selection_surface(
                prepared.colors.panel_background,
                prepared.colors.row_selected_background,
            );
            if transparency == 0.0 {
                assert_eq!(selected, prepared.colors.row_selected_background);
            } else {
                assert!(
                    selected.a > 0 && selected.a < 255,
                    "{mode:?} selected surface must transmit its backdrop at {transparency}: {selected:?}"
                );
                let host = prepared.colors.panel_background;
                assert!(
                    selected.source_over(host).contrast_ratio(host) >= 1.12,
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
fn surface_ladder_holds_its_order_from_the_default_setting_to_the_maximum() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let reference = builtin_chrome_base(appearance).opaque_presentation();
        for transparency in [0.05, 0.15, 0.35, 0.7, 1.0] {
            let mut preferences = AppearancePreferences::default();
            preferences.background.transparency = transparency;
            let materials = ResolvedWindowComposition::resolve(
                &preferences.background,
                CompositionCapabilities::new(true, true),
            )
            .materials;
            let pane = materials.paint(
                SurfaceRole::Surface,
                reference.background,
                reference.elevated_surface_background,
            );
            assert!(
                pane.a > 0,
                "{appearance:?} at {transparency}: Pane material disappeared"
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
                assert!(
                    hover.a > 0 && selected.a > hover.a,
                    "{appearance:?} at {transparency}: row ladder collapsed on the \
                     {host_name}: hover={} selection={}",
                    hover.a,
                    selected.a
                );
            }
        }
    }
}

/// Light's hierarchy is checked as rendered over a desktop: the navigation shell matches the
/// root, while raised surfaces and navigation selections lift across the transparency range.
#[test]
fn light_surfaces_separate_over_the_desktop_at_every_setting() {
    let reference = builtin_chrome_base(Appearance::Light).opaque_presentation();
    let desktop = Color::rgb(0x808080);
    for (transparency, minimum) in [(0.15, 11_u8), (0.35, 10), (0.7, 5), (1.0, 2)] {
        let mut preferences = AppearancePreferences::default();
        preferences.background.transparency = transparency;
        let materials = ResolvedWindowComposition::resolve(
            &preferences.background,
            CompositionCapabilities::new(true, true),
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
                "{name} at {transparency}: {step} levels of separation"
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
            "a Pane at {transparency} must stay clearly above the sheet: {pane:?} over {sheet:?}"
        );
    }
}

#[test]
fn transparency_rejects_invalid_numbers_and_defaults_for_documents_without_background_settings() {
    let mut preferences = AppearancePreferences::default();
    for invalid in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        preferences.background.transparency = invalid;
        assert!(preferences.validate().is_err());
    }
    let mut document = serde_json::to_value(SettingsDocument::default()).unwrap();
    document["preferences"]
        .as_object_mut()
        .unwrap()
        .remove("background");
    let parsed = parse_settings(&serde_json::to_vec(&document).unwrap()).unwrap();
    assert_eq!(parsed.preferences.background.transparency, 0.35);
    assert!(parsed.preferences.background.blur);
}

fn resolve(preferences: &AppearancePreferences) -> ResolvedAppearance {
    SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap()
}

#[test]
fn resolved_pane_caption_keeps_weight_400_while_chrome_caption_uses_retained_regular_weight() {
    let mut preferences = AppearancePreferences::default();
    preferences.chrome.typography.regular_weight = 500;
    preferences.chrome.typography.emphasis_weight = 700;
    preferences.chrome.typography.base_size = 24.0;
    let resolved = resolve(&preferences);
    let typography = &resolved.chrome.typography;

    assert_eq!(typography.caption.weight, 400);
    assert_eq!(
        typography.caption.primary_family,
        typography.body.primary_family
    );
    assert_eq!(typography.caption.size, 12.65 * (24.0 / 13.0));
    assert_eq!(typography.navigation.weight, 700);
    let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
    let caption = prepared
        .typography
        .style(crate::ui::chrome_typography::TextRole::Caption);
    let body = prepared
        .typography
        .style(crate::ui::chrome_typography::TextRole::Body);
    assert_eq!(caption.font.weight, gpui::FontWeight(500.0));
    assert_eq!(caption.font.family, body.font.family);
}

#[test]
fn builtin_list_hover_and_selection_match_without_aliasing_the_roles() {
    for appearance in [Appearance::Dark, Appearance::Light] {
        let mut colors = super::builtin::chrome_base(appearance);
        let selected = colors.ghost_element_selected;
        assert_ne!(colors.primary_background, selected);
        colors.apply(&ChromeColorOverrides {
            ghost_element_hover: Some(Color::rgba(0x12345680)),
            ..ChromeColorOverrides::default()
        });
        assert_eq!(colors.ghost_element_hover, Color::rgba(0x12345680));
        assert_eq!(colors.ghost_element_selected, selected);
        assert!(colors.validate().is_ok());
    }
}

#[test]
fn defaults_select_spaceterm_owned_schemes_in_both_appearances() {
    let catalog = SchemeCatalog::default();
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
            resolved.chrome.effective_scheme.as_str(),
            format!("builtin.spaceterm.chrome.{suffix}")
        );
        assert_eq!(
            resolved.terminal.effective_scheme.as_str(),
            format!("builtin.spaceterm.terminal.{suffix}")
        );
        let chrome = catalog.chrome(&resolved.chrome.effective_scheme).unwrap();
        let terminal = catalog
            .terminal(&resolved.terminal.effective_scheme)
            .unwrap();
        assert_eq!(chrome.name, terminal.name);
        assert_eq!(
            chrome.metadata.author.as_deref(),
            Some("SpaceTerm contributors")
        );
        assert_eq!(
            terminal.metadata.author.as_deref(),
            Some("SpaceTerm contributors")
        );
    }
}

#[test]
fn built_in_resting_surfaces_do_not_introduce_a_color_cast_at_any_transparency() {
    let catalog = SchemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        for step in 0..=100 {
            preferences.background.transparency = step as f32 / 100.0;
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
                ("pane rim", prepared.pane_rim()),
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
            // Neutral desktop light can change brightness, but must not acquire a scheme hue.
            for desktop in [0x000000, 0x808080, 0xffffff].map(Color::rgb) {
                let rendered = pane.source_over(sheet.source_over(desktop));
                assert_eq!(rendered.r, rendered.g);
                assert_eq!(rendered.g, rendered.b);
            }
        }
    }
}

#[test]
fn retired_builtin_selections_retain_overrides_without_missing_scheme_diagnostics() {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Dark;
    let chrome = SchemeId::builtin("builtin.vague-pro.chrome.dark");
    let terminal = SchemeId::builtin("builtin.vague-pro.terminal.dark");
    document.preferences.chrome.schemes.dark = chrome.clone();
    document.preferences.terminal.schemes.dark = terminal.clone();
    document.preferences.chrome.overrides.insert(
        chrome.clone(),
        ChromeColorOverrides {
            text: Some(Color::rgb(0xabcdef)),
            ..Default::default()
        },
    );
    document.preferences.terminal.overrides.insert(
        terminal.clone(),
        TerminalColorOverrides {
            foreground: Some(Color::rgb(0xfedcba)),
            ..Default::default()
        },
    );
    let loaded = parse_settings(&serde_json::to_vec(&document).unwrap()).unwrap();
    let resolved = resolve(&loaded.preferences);
    assert_eq!(resolved.chrome.colors.text, Color::rgb(0xabcdef));
    assert_eq!(resolved.terminal.colors.foreground, Color::rgb(0xfedcba));
    assert!(!loaded.preferences.chrome.overrides.contains_key(&chrome));
    assert!(
        !loaded
            .preferences
            .terminal
            .overrides
            .contains_key(&terminal)
    );
    assert!(!resolved.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic,
        AppearanceDiagnostic::ChromeSchemeUnavailable { .. }
            | AppearanceDiagnostic::TerminalSchemeUnavailable { .. }
    )));
    assert_eq!(
        parse_settings(&serde_json::to_vec(&loaded).unwrap()).unwrap(),
        loaded
    );
}

#[test]
fn color_and_typography_dimensions_resolve_independently() {
    let defaults = AppearancePreferences::default();
    let baseline = resolve(&defaults);

    let mut chrome_colors = defaults.clone();
    chrome_colors.chrome.overrides.insert(
        builtin_fallback_scheme(SchemeKind::Chrome, Appearance::Dark),
        ChromeColorOverrides {
            background: Some(Color::rgb(0x121314)),
            ..Default::default()
        },
    );
    let changed = resolve(&chrome_colors);
    assert_ne!(changed.chrome.colors, baseline.chrome.colors);
    assert_eq!(changed.chrome.typography, baseline.chrome.typography);
    assert_eq!(changed.terminal, baseline.terminal);

    let mut chrome_font = defaults.clone();
    chrome_font.chrome.typography.base_size = 14.0;
    let changed = resolve(&chrome_font);
    assert_ne!(changed.chrome.typography, baseline.chrome.typography);
    assert_eq!(changed.chrome.colors, baseline.chrome.colors);
    assert_eq!(changed.terminal, baseline.terminal);

    let mut terminal_colors = defaults.clone();
    terminal_colors.terminal.overrides.insert(
        builtin_fallback_scheme(SchemeKind::Terminal, Appearance::Dark),
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
            resolved.chrome.requested_scheme,
            *preferences.chrome.schemes.get(appearance)
        );
        assert_eq!(
            resolved.terminal.requested_scheme,
            *preferences.terminal.schemes.get(appearance)
        );
    }

    let preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    let catalog = SchemeCatalog::default();
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
    let catalog = SchemeCatalog::default();
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
    let missing = SchemeId::new("missing.chrome").unwrap();
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.chrome.schemes.light = missing.clone();
    preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: String::from("Unavailable Mono"),
    };
    let resolved = resolve(&preferences);
    assert_eq!(resolved.chrome.requested_scheme, missing);
    assert_eq!(
        resolved.chrome.effective_scheme.as_str(),
        "builtin.spaceterm.chrome.light"
    );
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::ChromeSchemeUnavailable {
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
            Some("Apple Color Emoji")
        );
        assert_eq!(
            descriptor
                .fallback_families
                .iter()
                .filter(|family| family.as_str() == "Apple Color Emoji")
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
    let resolved = SchemeCatalog::default()
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
        Some("Apple Color Emoji")
    );
    assert!(
        resolved
            .diagnostics
            .contains(&AppearanceDiagnostic::TerminalFontNotMonospace)
    );
}

#[test]
fn a_known_wrong_kind_or_classification_is_rejected() {
    let mut preferences = AppearancePreferences::default();
    preferences.chrome.schemes.dark = SchemeId::builtin("builtin.spaceterm.terminal.dark");
    assert!(matches!(
        SchemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::WrongSchemeKind)
    ));

    preferences.mode = AppearanceMode::Light;
    preferences.chrome.schemes.light = SchemeId::builtin("builtin.spaceterm.chrome.dark");
    assert!(matches!(
        SchemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::AppearanceMismatch)
    ));

    let mut document = SettingsDocument::default();
    document.preferences.chrome.overrides.insert(
        SchemeId::builtin("builtin.spaceterm.terminal.dark"),
        ChromeColorOverrides::default(),
    );
    assert!(matches!(
        document.validate(),
        Err(SettingsDocumentError::InvalidPreferences)
    ));

    let mut document = SettingsDocument::default();
    document.preferences.terminal.overrides.insert(
        SchemeId::builtin("builtin.spaceterm.chrome.dark"),
        TerminalColorOverrides::default(),
    );
    assert!(matches!(
        document.validate(),
        Err(SettingsDocumentError::InvalidPreferences)
    ));
}

fn reset_fixture() -> SettingsDocument {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Light;
    document.preferences.chrome.schemes.light = SchemeId::new("missing.reset.chrome").unwrap();
    document.preferences.chrome.typography.family = ChromeFontFamily::Named {
        family: String::from("Helvetica Neue"),
    };
    document.preferences.chrome.typography.base_size = 24.0;
    document.preferences.chrome.typography.regular_weight = 900;
    document.preferences.chrome.typography.emphasis_weight = 800;
    document.preferences.chrome.typography.heading_weight = 700;
    document.preferences.chrome.density = ChromeDensity::Comfortable;
    document.preferences.chrome.overrides.insert(
        SchemeId::builtin("builtin.spaceterm.chrome.light"),
        ChromeColorOverrides::complete(&ChromeColors::default()),
    );

    document.preferences.terminal.schemes.light = SchemeId::new("missing.reset.terminal").unwrap();
    document.preferences.terminal.typography.family = TerminalFontFamily::Named {
        family: String::from("Menlo"),
    };
    document.preferences.terminal.typography.base_size = 32.0;
    document.preferences.terminal.typography.regular_weight = 800;
    document.preferences.terminal.typography.bold_weight = 900;
    document.preferences.terminal.typography.line_height = 2.0;
    document.preferences.terminal.typography.italic = false;
    document.preferences.terminal.rendering.bold_as_bright = false;
    document.preferences.terminal.overrides.insert(
        SchemeId::builtin("builtin.spaceterm.terminal.light"),
        TerminalColorOverrides::complete(&TerminalColors::default()),
    );

    document
        .custom_schemes
        .push(CustomScheme::Chrome(Box::new(ChromeScheme {
            window_background: None,
            id: SchemeId::new("custom.reset-fixture").unwrap(),
            name: String::from("Reset Fixture"),
            appearance: Appearance::Dark,
            metadata: SchemeMetadata::default(),
            colors: ChromeColorOverrides::default(),
        })));
    document.validate().unwrap();
    document
}

/// A scheme row's reset restores the scheme, not the mode.
///
/// The appearance mode belongs to the one control spanning both surfaces, so restoring one
/// surface's scheme must not move that surface to a different mode and leave the other behind.
#[test]
fn resetting_a_scheme_slot_keeps_the_mode_and_other_slots() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    preferences.chrome.schemes.light = SchemeId::new("missing.changed.chrome").unwrap();
    let dark = preferences.chrome.schemes.dark.clone();
    let terminal = preferences.terminal.schemes.clone();

    preferences.reset(ResetTarget::ChromeScheme(Appearance::Light));

    assert_eq!(preferences.mode, AppearanceMode::Auto);
    assert_eq!(
        preferences.chrome.schemes.light,
        AppearancePreferences::default().chrome.schemes.light
    );
    assert_eq!(preferences.chrome.schemes.dark, dark);
    assert_eq!(preferences.terminal.schemes, terminal);
}

#[test]
fn resetting_the_mode_preserves_every_scheme_slot() {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Auto,
        ..Default::default()
    };
    let chrome = preferences.chrome.schemes.clone();
    let terminal = preferences.terminal.schemes.clone();

    preferences.reset(ResetTarget::AppearanceMode);

    assert_eq!(preferences.mode, AppearanceMode::Dark);
    assert_eq!(preferences.chrome.schemes, chrome);
    assert_eq!(preferences.terminal.schemes, terminal);
}

#[test]
fn every_individual_preference_reset_changes_only_its_field() {
    type ExpectedEdit = fn(&mut AppearancePreferences);
    let defaults = AppearancePreferences::default();
    let cases: Vec<(ResetTarget, ExpectedEdit)> = vec![
        (ResetTarget::AppearanceMode, |value| {
            value.mode = AppearancePreferences::default().mode;
        }),
        (ResetTarget::ChromeScheme(Appearance::Light), |value| {
            value.chrome.schemes.light = AppearancePreferences::default().chrome.schemes.light;
        }),
        (ResetTarget::ChromeFontFamily, |value| {
            value.chrome.typography.family =
                AppearancePreferences::default().chrome.typography.family;
        }),
        (ResetTarget::ChromeBaseSize, |value| {
            value.chrome.typography.base_size =
                AppearancePreferences::default().chrome.typography.base_size;
        }),
        (ResetTarget::ChromeRegularWeight, |value| {
            value.chrome.typography.regular_weight = AppearancePreferences::default()
                .chrome
                .typography
                .regular_weight;
        }),
        (ResetTarget::ChromeEmphasisWeight, |value| {
            value.chrome.typography.emphasis_weight = AppearancePreferences::default()
                .chrome
                .typography
                .emphasis_weight;
        }),
        (ResetTarget::ChromeHeadingWeight, |value| {
            value.chrome.typography.heading_weight = AppearancePreferences::default()
                .chrome
                .typography
                .heading_weight;
        }),
        (ResetTarget::TerminalScheme(Appearance::Light), |value| {
            value.terminal.schemes.light = AppearancePreferences::default().terminal.schemes.light;
        }),
        (ResetTarget::TerminalFontFamily, |value| {
            value.terminal.typography.family =
                AppearancePreferences::default().terminal.typography.family;
        }),
        (ResetTarget::TerminalBaseSize, |value| {
            value.terminal.typography.base_size = AppearancePreferences::default()
                .terminal
                .typography
                .base_size;
        }),
        (ResetTarget::TerminalRegularWeight, |value| {
            value.terminal.typography.regular_weight = AppearancePreferences::default()
                .terminal
                .typography
                .regular_weight;
        }),
        (ResetTarget::TerminalBoldWeight, |value| {
            value.terminal.typography.bold_weight = AppearancePreferences::default()
                .terminal
                .typography
                .bold_weight;
        }),
        (ResetTarget::TerminalLineHeight, |value| {
            value.terminal.typography.line_height = AppearancePreferences::default()
                .terminal
                .typography
                .line_height;
        }),
        (ResetTarget::TerminalItalic, |value| {
            value.terminal.typography.italic =
                AppearancePreferences::default().terminal.typography.italic;
        }),
        (ResetTarget::TerminalBoldAsBright, |value| {
            value.terminal.rendering.bold_as_bright = AppearancePreferences::default()
                .terminal
                .rendering
                .bold_as_bright;
        }),
    ];
    assert_eq!(cases.len(), 15);

    for (target, expected_edit) in cases {
        let mut actual = reset_fixture();
        let retained_schemes = actual.custom_schemes.clone();
        let mut expected = actual.preferences.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.preferences, expected, "{target:?}");
        assert_eq!(actual.custom_schemes, retained_schemes, "{target:?}");
    }

    assert_ne!(reset_fixture().preferences, defaults);
}

#[test]
fn group_and_all_resets_have_exact_scope_and_retain_custom_schemes() {
    type ExpectedEdit = fn(&mut AppearancePreferences);
    let cases: Vec<(ResetTarget, ExpectedEdit)> = vec![
        (ResetTarget::ChromeColors, |value| {
            let defaults = AppearancePreferences::default();
            value.chrome.schemes = defaults.chrome.schemes;
            value.chrome.overrides.clear();
        }),
        (ResetTarget::ChromeTypography, |value| {
            value.chrome.typography = AppearancePreferences::default().chrome.typography;
        }),
        (ResetTarget::ChromeDensity, |value| {
            value.chrome.density = AppearancePreferences::default().chrome.density;
        }),
        (ResetTarget::TerminalColors, |value| {
            let defaults = AppearancePreferences::default();
            value.terminal.schemes = defaults.terminal.schemes;
            value.terminal.overrides.clear();
        }),
        (ResetTarget::TerminalTypography, |value| {
            value.terminal.typography = AppearancePreferences::default().terminal.typography;
        }),
        (ResetTarget::TerminalRendering, |value| {
            value.terminal.rendering = AppearancePreferences::default().terminal.rendering;
        }),
        (ResetTarget::AllAppearance, |value| {
            *value = AppearancePreferences::default();
        }),
    ];

    for (target, expected_edit) in cases {
        let mut actual = reset_fixture();
        let retained_schemes = actual.custom_schemes.clone();
        let mut expected = actual.preferences.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.preferences, expected, "{target:?}");
        assert_eq!(actual.custom_schemes, retained_schemes, "{target:?}");
    }
}

/// `reset_all` is the document's factory reset, so it goes further than any [`ResetTarget`]: the
/// imported catalog empties with the preferences, and the identity fields that order the write
/// against concurrent editors carry forward untouched.
#[test]
fn resetting_the_whole_document_empties_the_catalog_and_keeps_its_identity() {
    let mut document = reset_fixture();
    document.revision = 17;
    let schema_version = document.schema_version;
    assert!(
        !document.custom_schemes.is_empty(),
        "the fixture should install one scheme"
    );

    document.reset_all();

    assert_eq!(
        document.preferences,
        AppearancePreferences::default(),
        "every preference should return to its default"
    );
    assert!(
        document.custom_schemes.is_empty(),
        "the imported catalog should empty"
    );
    assert_eq!(
        document.revision, 17,
        "the write order should carry forward"
    );
    assert_eq!(document.schema_version, schema_version);
    document.validate().unwrap();
}

/// A selection naming an imported scheme is valid only while that scheme is installed, so
/// emptying the catalog and defaulting the preferences have to land in the same edit.
#[test]
fn resetting_the_whole_document_releases_a_selected_imported_scheme() {
    let mut document = reset_fixture();
    let imported = document.custom_schemes[0].id().clone();
    document.preferences.chrome.schemes.dark = imported.clone();
    document.validate().unwrap();

    document.reset_all();

    assert_ne!(document.preferences.chrome.schemes.dark, imported);
    document.validate().unwrap();
}

#[test]
fn every_color_role_can_be_removed_without_changing_other_overrides() {
    macro_rules! role_names {
        ($($field:ident),+ $(,)?) => { &[ $(stringify!($field)),+ ] };
    }
    let chrome_roles: &[&'static str] = chrome_color_fields!(role_names);
    let terminal_roles: &[&'static str] = &[
        "foreground",
        "background",
        "normal",
        "bright",
        "dim",
        "bright_foreground",
        "dim_foreground",
        "cursor",
        "cursor_text",
        "selection_background",
        "selection_foreground",
        "find_match_background",
        "find_match_foreground",
        "find_active_match_background",
        "find_active_match_foreground",
        "hyperlink",
        "visual_bell",
    ];
    let chrome_id = SchemeId::builtin("builtin.spaceterm.chrome.light");
    let terminal_id = SchemeId::builtin("builtin.spaceterm.terminal.light");

    for role in chrome_roles {
        let mut actual = reset_fixture();
        let retained_schemes = actual.custom_schemes.clone();
        let before_terminal = actual.preferences.terminal.clone();
        let mut expected =
            serde_json::to_value(actual.preferences.chrome.overrides.get(&chrome_id).unwrap())
                .unwrap();
        expected.as_object_mut().unwrap().remove(*role);
        actual
            .reset(ResetTarget::chrome_color_override(chrome_id.clone(), role).unwrap())
            .unwrap();
        assert_eq!(
            serde_json::to_value(actual.preferences.chrome.overrides.get(&chrome_id).unwrap())
                .unwrap(),
            expected,
            "chrome role {role}"
        );
        assert_eq!(
            actual.preferences.terminal, before_terminal,
            "chrome role {role}"
        );
        assert_eq!(
            actual.custom_schemes, retained_schemes,
            "chrome role {role}"
        );
    }

    for role in terminal_roles {
        let mut actual = reset_fixture();
        let retained_schemes = actual.custom_schemes.clone();
        let before_chrome = actual.preferences.chrome.clone();
        let mut expected = serde_json::to_value(
            actual
                .preferences
                .terminal
                .overrides
                .get(&terminal_id)
                .unwrap(),
        )
        .unwrap();
        expected.as_object_mut().unwrap().remove(*role);
        actual
            .reset(ResetTarget::terminal_color_override(terminal_id.clone(), role).unwrap())
            .unwrap();
        assert_eq!(
            serde_json::to_value(
                actual
                    .preferences
                    .terminal
                    .overrides
                    .get(&terminal_id)
                    .unwrap()
            )
            .unwrap(),
            expected,
            "terminal role {role}"
        );
        assert_eq!(
            actual.preferences.chrome, before_chrome,
            "terminal role {role}"
        );
        assert_eq!(
            actual.custom_schemes, retained_schemes,
            "terminal role {role}"
        );
    }

    assert!(ResetTarget::chrome_color_override(chrome_id, "not_a_role").is_none());
    assert!(ResetTarget::terminal_color_override(terminal_id, "not_a_role").is_none());

    let mut sparse = SettingsDocument::default();
    let chrome_id = SchemeId::builtin("builtin.spaceterm.chrome.dark");
    sparse.preferences.chrome.overrides.insert(
        chrome_id.clone(),
        ChromeColorOverrides {
            background: Some(Color::rgb(0x101010)),
            ..ChromeColorOverrides::default()
        },
    );
    sparse
        .reset(ResetTarget::chrome_color_override(chrome_id.clone(), "background").unwrap())
        .unwrap();
    assert!(!sparse.preferences.chrome.overrides.contains_key(&chrome_id));

    let terminal_id = SchemeId::builtin("builtin.spaceterm.terminal.dark");
    sparse.preferences.terminal.overrides.insert(
        terminal_id.clone(),
        TerminalColorOverrides {
            cursor_text: OptionalColorOverride::None,
            ..TerminalColorOverrides::default()
        },
    );
    sparse
        .reset(ResetTarget::terminal_color_override(terminal_id.clone(), "cursor_text").unwrap())
        .unwrap();
    assert!(
        !sparse
            .preferences
            .terminal
            .overrides
            .contains_key(&terminal_id)
    );
}

#[test]
fn native_settings_are_canonical_strict_and_round_trip() {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Auto;
    document.preferences.chrome.schemes.light = SchemeId::new("missing.chrome.light").unwrap();
    document.preferences.chrome.schemes.dark = SchemeId::new("missing.chrome.dark").unwrap();
    document.preferences.terminal.schemes.light = SchemeId::new("missing.terminal.light").unwrap();
    document.preferences.terminal.schemes.dark = SchemeId::new("missing.terminal.dark").unwrap();
    let encoded = export_settings(&document).unwrap();
    let encoded_value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(encoded_value["schema_version"], 2);
    assert_eq!(parse_settings(encoded.as_bytes()).unwrap(), document);
    let unsupported = encoded.replacen("\"schema_version\": 2", "\"schema_version\": 1", 1);
    assert!(matches!(
        parse_settings(unsupported.as_bytes()),
        Err(SettingsDocumentError::UnsupportedVersion)
    ));
    assert!(matches!(parse_settings(br#"{"schema_version":2,"schema_version":2,"revision":0,"preferences":{},"custom_schemes":[]}"#),
        Err(SettingsDocumentError::DuplicateKey)));

    let unknown = encoded.replacen(
        "\"revision\": 0,",
        "\"revision\": 0,\n  \"unknown\": true,",
        1,
    );
    assert!(matches!(
        parse_settings(unknown.as_bytes()),
        Err(SettingsDocumentError::InvalidJson)
    ));
}

#[test]
fn invalid_bounds_and_protocol_alpha_are_rejected() {
    let mut document = SettingsDocument::default();
    document.preferences.chrome.typography.base_size = f32::NAN;
    assert!(matches!(
        export_settings(&document),
        Err(SettingsDocumentError::InvalidPreferences)
    ));

    let custom = CustomScheme::Terminal(Box::new(TerminalScheme {
        id: SchemeId::new("custom.alpha").unwrap(),
        name: String::from("Alpha"),
        appearance: Appearance::Dark,
        metadata: SchemeMetadata::default(),
        colors: TerminalColorOverrides {
            foreground: Some(Color::rgba(0xffffff80)),
            ..Default::default()
        },
    }));
    let document = SettingsDocument {
        custom_schemes: vec![custom],
        ..SettingsDocument::default()
    };
    assert!(matches!(
        export_settings(&document),
        Err(SettingsDocumentError::InvalidCatalog)
    ));
}

#[test]
fn catalog_batch_install_is_atomic_and_revision_checked() {
    let scheme = CustomScheme::Chrome(Box::new(ChromeScheme {
        window_background: None,
        id: SchemeId::new("custom.blue").unwrap(),
        name: String::from("Blue"),
        appearance: Appearance::Dark,
        metadata: SchemeMetadata::default(),
        colors: ChromeColorOverrides::default(),
    }));
    let mut catalog = SchemeCatalog::default();
    assert_eq!(
        catalog
            .install_batch(std::slice::from_ref(&scheme), 0, &BTreeSet::new())
            .unwrap(),
        vec![scheme.id().clone()]
    );
    assert_eq!(catalog.revision(), 1);
    assert!(matches!(
        catalog.install_batch(&[scheme], 0, &BTreeSet::new()),
        Err(CatalogError::RevisionConflict)
    ));
}

#[test]
fn zed_import_uses_explicit_candidate_and_deterministic_kind_ids() {
    let bytes = include_bytes!("fixtures/vague-pro/theme.json");
    let candidates = list_zed_candidates(bytes).unwrap();
    assert_eq!(candidates.len(), 1);
    let first = import_zed(
        bytes,
        candidates[0].index,
        &[ZedImportKind::Chrome, ZedImportKind::Terminal],
    )
    .unwrap();
    let again = import_zed(
        bytes,
        candidates[0].index,
        &[ZedImportKind::Chrome, ZedImportKind::Terminal],
    )
    .unwrap();
    assert_eq!(first, again);
    assert_eq!(first.len(), 2);
    assert!(first[0].id().as_str().ends_with(".chrome"));
    assert!(first[1].id().as_str().ends_with(".terminal"));

    let duplicate = br##"{
        "themes": [
            {"name":"Duplicate","appearance":"dark","style":{}},
            {"name":"Duplicate","appearance":"dark","style":{}}
        ]
    }"##;
    assert!(list_zed_candidates(duplicate).is_err());
    assert!(import_zed(duplicate, 0, &[ZedImportKind::Chrome]).is_err());
}

#[test]
fn zed_list_states_remain_distinct_through_native_export_and_resolution() {
    let bytes = br##"{"themes":[{"name":"Distinct states","appearance":"dark","style":{
        "ghost_element.hover":"#12345680",
        "ghost_element.selected":"#abcdefcc",
        "tab.active_background":"#334455"
    }}]}"##;
    let schemes = import_zed(bytes, 0, &[ZedImportKind::Chrome]).unwrap();
    let id = schemes[0].id().clone();
    let mut catalog = SchemeCatalog::default();
    catalog
        .install_batch(&schemes, 0, &BTreeSet::new())
        .unwrap();
    let output = export_schemes(&catalog, &[(SchemeKind::Chrome, id.clone())]).unwrap();
    let exported = parse_color_document(output.as_bytes()).unwrap();
    let mut reloaded = SchemeCatalog::default();
    reloaded
        .install_batch(&exported.schemes, 0, &BTreeSet::new())
        .unwrap();
    let mut preferences = AppearancePreferences::default();
    preferences.chrome.schemes.dark = id;
    let resolved = reloaded
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(
        resolved.chrome.colors.ghost_element_hover,
        Color::rgba(0x12345680)
    );
    assert_eq!(
        resolved.chrome.colors.ghost_element_selected,
        Color::rgba(0xabcdefcc)
    );
    assert_eq!(
        resolved.chrome.colors.tab_active_background,
        Color::rgb(0x334455)
    );
}

#[test]
fn native_examples_and_complete_export_follow_the_runtime_contract() {
    let accepted = include_bytes!("../../docs/appearance-examples/partial-color-schemes.json");
    assert_eq!(parse_color_document(accepted).unwrap().schemes.len(), 2);
    let rejected = include_bytes!("../../docs/appearance-examples/rejected-unknown-role.json");
    assert!(parse_color_document(rejected).is_err());

    let catalog = SchemeCatalog::default();
    let output = export_schemes(
        &catalog,
        &[
            (
                SchemeKind::Chrome,
                SchemeId::builtin("builtin.spaceterm.chrome.dark"),
            ),
            (
                SchemeKind::Terminal,
                SchemeId::builtin("builtin.spaceterm.terminal.light"),
            ),
        ],
    )
    .unwrap();
    let encoded: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(encoded["schema_version"], 1);
    let exported = parse_color_document(output.as_bytes()).unwrap();
    match &exported.schemes[0] {
        CustomScheme::Chrome(scheme) => assert!(scheme.colors.background.is_some()),
        _ => panic!("first exported scheme must be chrome"),
    }
    match &exported.schemes[1] {
        CustomScheme::Terminal(scheme) => {
            assert!(scheme.colors.normal.is_some());
            assert!(matches!(
                scheme.colors.cursor_text,
                OptionalColorOverride::None
            ));
        }
        _ => panic!("second exported scheme must be terminal"),
    }
}

#[test]
fn color_encoding_accepts_short_forms_and_exports_long_rgba() {
    let color: Color = serde_json::from_str("\"#abc\"").unwrap();
    assert_eq!(color, Color::rgb(0xaabbcc));
    assert_eq!(serde_json::to_string(&color).unwrap(), "\"#aabbccff\"");
}
