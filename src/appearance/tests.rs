use std::collections::BTreeSet;

use super::*;
use crate::settings::{SettingsDocument, SettingsDocumentError, export_settings, parse_settings};

#[test]
fn floating_backdrop_alpha_limit_only_opens_over_an_effective_native_backdrop() {
    let mut preferences = AppearancePreferences::default();
    preferences.window.transparency = 1.0;

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

    preferences.window.transparency = 0.0;
    let opaque = ResolvedWindowComposition::resolve(
        &preferences.window,
        CompositionCapabilities::new(true, true),
        ChromeTone::Dark,
    );
    assert_eq!(opaque.materials.floating_backdrop_alpha_limit(), 1.0);
}

#[test]
fn transparency_resolves_endpoints_in_both_modes_without_changing_theme_colors() {
    let catalog = ThemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        let opaque = resolve(&preferences);
        for transparency in [0.0, 0.15, 0.35, 1.0] {
            preferences.window.transparency = transparency;
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
            if transparency == 0.0 {
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
                    "{mode:?} at {transparency}: an elevated resting surface retains more color than the sheet"
                );
                if transparency == 1.0 {
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

/// The Pane material of one appearance, prepared at one Transparency Setting.
fn prepared_appearance(mode: AppearanceMode, transparency: f32) -> ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode,
        ..Default::default()
    };
    preferences.window.transparency = transparency;
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

/// A Dark Pane reads as the window's own surface, one quiet step above the window root.
///
/// The step is measured against the opaque theme reference, which every material derives from,
/// so it holds whatever the desktop behind the window happens to be. The material keeps the step
/// subtle as the window transmits.
#[test]
fn dark_pane_rests_one_subtle_step_above_the_window_root() {
    for transparency in [0.0, 0.05, 0.15, 0.35, 0.7, 1.0] {
        let resolved = prepared_appearance(AppearanceMode::Dark, transparency);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let root = prepared.colors.background;
        let pane = prepared
            .pane_surface(resolved.terminal.colors.background)
            .source_over(root);

        assert!(
            [(pane.r, root.r), (pane.g, root.g), (pane.b, root.b)]
                .into_iter()
                .all(|(pane, root)| pane > root),
            "a Dark Pane stays lighter than the window root at transparency {transparency}: pane={pane:?}",
        );
        let step = root.contrast_ratio(pane);
        assert!(
            (1.015..=1.07).contains(&step),
            "a Dark Pane stays within one subtle step of the window root at transparency {transparency}: step={step}, pane={pane:?}",
        );
    }
}

/// The Transparency Setting owns the Terminal surface as much as it owns the chrome.
///
/// A Pane covers the largest part of the window, so a Pane that held its own backing would answer
/// the Setting with a slab the reader never asked for. It transmits with the window instead, and
/// keeps only the sliver of ink its step below the root costs.
#[test]
fn dark_pane_transmits_what_the_transparency_setting_asks() {
    let settings = [0.0_f32, 0.15, 0.35, 0.7, 1.0];
    let mut previous: Option<f64> = None;
    for transparency in settings {
        let resolved = prepared_appearance(AppearanceMode::Dark, transparency);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
        let pane = prepared.pane_surface(resolved.terminal.colors.background);
        let transmitted = |fill: Color| 1.0 - f64::from(fill.a) / 255.0;
        let chrome = transmitted(sheet);
        let terminal = chrome * transmitted(pane);

        if let Some(previous) = previous {
            assert!(
                terminal > previous,
                "a Dark Pane admits more desktop as the Setting rises: {terminal} at transparency {transparency} after {previous}",
            );
        }
        previous = Some(terminal);
        assert!(
            terminal >= chrome * 0.8,
            "a Dark Pane admits nearly what the chrome admits at transparency {transparency}: terminal={terminal}, chrome={chrome}",
        );
        assert!(
            terminal < chrome || transparency == 0.0,
            "a Dark Pane stays denser than the chrome at transparency {transparency}",
        );
    }
}

/// An authored Terminal background that states a color keeps it; a neutral one joins the ladder.
#[test]
fn dark_pane_keeps_a_stated_terminal_background_apart_from_the_neutral_ladder() {
    let resolved = prepared_appearance(AppearanceMode::Dark, 0.35);
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

    for transparency in [0.0, 0.35, 1.0] {
        let mut preferences = AppearancePreferences {
            mode: AppearanceMode::Light,
            ..Default::default()
        };
        preferences.window.transparency = transparency;
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
            (4, 4),
            "Light navigation should retain its authored chip lift at {transparency}"
        );
    }
}

/// A Light Pane reads as the window's own surface, one quiet step above the chrome around it.
///
/// The step is measured against the opaque theme reference, which every material derives from.
/// It is the full authored 1.21 while the window is opaque and narrows as glass engages, because
/// a bright rung may spend no more than the ladder ceiling once the window transmits. Over a real
/// desktop darker than the theme, the same overlay covers more distance and the step widens
/// again, so the narrowed reference figure is the floor rather than the typical case.
#[test]
fn light_pane_rests_one_subtle_step_above_the_window_root() {
    for transparency in [0.0, 0.05, 0.15, 0.35, 0.7, 1.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, transparency);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let root = prepared.colors.background;
        let pane = prepared
            .pane_surface(resolved.terminal.colors.background)
            .source_over(root);

        assert!(
            [(pane.r, root.r), (pane.g, root.g), (pane.b, root.b)]
                .into_iter()
                .all(|(pane, root)| pane > root),
            "a Light Pane stays brighter than the window root at transparency {transparency}: pane={pane:?}",
        );
        let step = pane.contrast_ratio(root);
        assert!(
            (1.035..=1.24).contains(&step),
            "a Light Pane stays within one subtle step of the window root at transparency {transparency}: step={step}, pane={pane:?}",
        );
    }
}

/// The Transparency Setting owns the Light Terminal surface as much as it owns the chrome.
///
/// A Pane covers the largest part of the window. Holding its authored step against the opaque
/// host would cost an almost opaque white that the Setting never reaches, which is the slab the
/// Light window used to paint. It transmits with the window instead and keeps only the overlay
/// its step above the root costs. That overlay is wider than a Dark Pane's, because a bright
/// theme needs more ink to say the same thing, so a Light Pane admits a smaller share of what
/// the chrome admits.
#[test]
fn light_pane_transmits_what_the_transparency_setting_asks() {
    let settings = [0.0_f32, 0.15, 0.35, 0.7, 1.0];
    let mut previous: Option<f64> = None;
    for transparency in settings {
        let resolved = prepared_appearance(AppearanceMode::Light, transparency);
        let prepared = crate::ui::appearance::ChromeAppearance::prepare(&resolved.chrome);
        let sheet = prepared.surface(SurfaceRole::Sheet, prepared.colors.background);
        let pane = prepared.pane_surface(resolved.terminal.colors.background);
        let transmitted = |fill: Color| 1.0 - f64::from(fill.a) / 255.0;
        let chrome = transmitted(sheet);
        let terminal = chrome * transmitted(pane);

        if let Some(previous) = previous {
            assert!(
                terminal > previous,
                "a Light Pane admits more desktop as the Setting rises: {terminal} at transparency {transparency} after {previous}",
            );
        }
        previous = Some(terminal);
        // Bright Chrome reconstructs its Pane with near-white ink over a near-white root, which
        // costs more coverage than a dark rung's sliver, so this bound sits below the Dark one
        // rather than sharing it.
        assert!(
            terminal >= chrome * 0.6,
            "a Light Pane admits nearly what the chrome admits at transparency {transparency}: terminal={terminal}, chrome={chrome}",
        );
        assert!(
            terminal < chrome || transparency == 0.0,
            "a Light Pane stays denser than the chrome at transparency {transparency}",
        );
    }
}

/// A Light selected chip spends one overlay for its step and transmits the rest.
///
/// This is the shape of the bug it guards: a chip that holds its step against the opaque host
/// keeps its ink while the shell under it goes on fading, so what it renders climbs with the
/// Setting until the chip is the loudest thing in a window that was asked for glass. The same
/// chip painted as a resting surface thins as the Setting rises and stays within reach of the
/// authored 1.21. It does drift upward over a desktop darker than the theme, because equal ink
/// buys a wider luminance ratio the darker its backing is, but it drifts within a band instead
/// of leaving one: over this desktop the pinned chip reached 2.0 at the default Setting and 4.0
/// above it, where a chip now reads 1.92 at that Setting and peaks at 2.31 with the shell most of
/// the way cleared.
#[test]
fn light_selected_navigation_keeps_one_step_across_the_setting() {
    use spaceterm_ui::ControlHost;

    let desktop = Color::rgb(0x2b3a55);
    for transparency in [0.0, 0.15, 0.35, 0.7, 1.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, transparency);
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
                "a Light {name} holds one step from its shell at transparency {transparency}: step={step}, chip={chip:?} over {shell:?}",
            );
            assert!(
                chip.r > shell.r,
                "a Light {name} lifts from its shell at transparency {transparency}",
            );
            let ink = appearance.selection_surface(host, fill).a;
            assert_eq!(
                ink == 255,
                transparency == 0.0,
                "a Light {name} transmits at transparency {transparency}: ink={ink}",
            );
        }
    }
}

/// The Light Terminal backing is the Terminal's own color under the window material, with no
/// elevation rung of its own, and window activation does not change it.
#[test]
fn light_terminal_backing_applies_transparency_without_an_elevation_tint() {
    let mut previous: Option<u8> = None;
    for transparency in [0.0, 0.05, 0.15, 0.35, 0.7, 1.0] {
        let resolved = prepared_appearance(AppearanceMode::Light, transparency);
        let background = resolved.terminal.colors.background;
        let (active, inactive) =
            crate::ui::appearance::ChromeAppearance::prepare_variants(&resolved.chrome);
        let alpha = active.pane_surface(background).a;
        for prepared in [active, inactive] {
            let pane = prepared.pane_surface(background);
            assert_eq!(
                pane,
                prepared.surface(SurfaceRole::Surface, background),
                "Light Terminal at transparency {transparency}, active={}",
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
                "the Light Terminal backing thins as the Setting rises: {alpha} at transparency {transparency} after {previous}",
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
fn selected_surfaces_follow_transparency_in_both_appearances() {
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            let mut preferences = AppearancePreferences {
                mode,
                ..Default::default()
            };
            preferences.window.transparency = transparency;
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
            if transparency == 0.0 {
                assert_eq!(selected, prepared.colors.row_selected_background);
            } else {
                assert!(
                    selected.a > 0 && selected.a < 255,
                    "{mode:?} selected surface must transmit its backdrop at {transparency}: {selected:?}"
                );
                // A transmitting selection is read against the shell the window actually
                // renders, not against the opaque reference it was solved from, so the bound it
                // can promise there is the ink it spends rather than a fixed ratio.
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
fn surface_ladder_holds_its_order_from_the_default_setting_to_the_maximum() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let reference = builtin_chrome_base(appearance).opaque_presentation();
        for transparency in [0.05, 0.15, 0.35, 0.7, 1.0] {
            let mut preferences = AppearancePreferences::default();
            preferences.window.transparency = transparency;
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
                if host == reference.row_selected_background {
                    assert_eq!(selected.a, 0, "equal host and fill need no overlay");
                    continue;
                }
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
        preferences.window.transparency = transparency;
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
fn transparency_rejects_invalid_numbers() {
    let mut preferences = AppearancePreferences::default();
    for invalid in [-0.01, 1.01, f32::NAN, f32::INFINITY] {
        preferences.window.transparency = invalid;
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
fn built_in_resting_surfaces_do_not_introduce_a_color_cast_at_any_transparency() {
    let catalog = ThemeCatalog::default();
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let mut preferences = AppearancePreferences {
            mode,
            ..Default::default()
        };
        for step in 0..=100 {
            preferences.window.transparency = step as f32 / 100.0;
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

#[test]
fn a_terminal_theme_must_match_its_slot_appearance() {
    let mut document = SettingsDocument::default();
    document.preferences.terminal.themes.light = super::builtin::dark_terminal_id();
    document.preferences.mode = AppearanceMode::Light;
    assert_eq!(
        document.validate(),
        Err(SettingsDocumentError::InvalidPreferences)
    );
    assert!(matches!(
        ThemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &document.preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::AppearanceMismatch)
    ));
}

fn reset_fixture() -> SettingsDocument {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Light;
    document.preferences.window.density = ChromeDensity::Comfortable;
    document.preferences.window.transparency = 0.8;
    document.preferences.window.blur = false;
    document.preferences.terminal.themes.light = ThemeId::new("custom.reset-light").unwrap();
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
        ThemeId::builtin("builtin.spaceterm.light"),
        TerminalColorOverrides::complete(&TerminalColors::default()),
    );

    document.terminal_themes.push(TerminalTheme {
        id: ThemeId::new("custom.reset-fixture").unwrap(),
        name: String::from("Reset Fixture"),
        appearance: Appearance::Dark,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides::default(),
    });
    document.terminal_themes.push(TerminalTheme {
        id: ThemeId::new("custom.reset-light").unwrap(),
        name: String::from("Reset Light"),
        appearance: Appearance::Light,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides::default(),
    });
    document.validate().unwrap();
    document
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
fn every_individual_preference_reset_changes_only_its_field() {
    type ExpectedEdit = fn(&mut AppearancePreferences);
    let defaults = AppearancePreferences::default();
    let cases: Vec<(ResetTarget, ExpectedEdit)> = vec![
        (ResetTarget::AppearanceMode, |value| {
            value.mode = AppearancePreferences::default().mode;
        }),
        (ResetTarget::Density, |value| {
            value.window.density = ChromeDensity::Compact
        }),
        (ResetTarget::Transparency, |value| {
            value.window.transparency = 0.35
        }),
        (ResetTarget::Blur, |value| value.window.blur = true),
        (ResetTarget::TerminalTheme(Appearance::Light), |value| {
            value.terminal.themes.light = AppearancePreferences::default().terminal.themes.light;
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
    assert_eq!(cases.len(), 12);

    for (target, expected_edit) in cases {
        let mut actual = reset_fixture();
        let retained_themes = actual.terminal_themes.clone();
        let mut expected = actual.preferences.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.preferences, expected, "{target:?}");
        assert_eq!(actual.terminal_themes, retained_themes, "{target:?}");
    }

    assert_ne!(reset_fixture().preferences, defaults);
}

#[test]
fn group_and_all_resets_have_exact_scope_and_retain_color_themes() {
    type ExpectedEdit = fn(&mut AppearancePreferences);
    let cases: Vec<(ResetTarget, ExpectedEdit)> = vec![
        (ResetTarget::Density, |value| {
            value.window.density = AppearancePreferences::default().window.density;
        }),
        (ResetTarget::TerminalColors, |value| {
            let defaults = AppearancePreferences::default();
            value.terminal.themes = defaults.terminal.themes;
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
        let retained_themes = actual.terminal_themes.clone();
        let mut expected = actual.preferences.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.preferences, expected, "{target:?}");
        assert_eq!(actual.terminal_themes, retained_themes, "{target:?}");
    }
}

/// `reset_all` is the document's factory reset, so it goes further than any [`ResetTarget`]: the
/// imported catalog empties with the preferences, and the identity fields that order the write
/// against concurrent editors carry forward untouched.
#[test]
fn resetting_the_whole_document_empties_the_catalog_and_keeps_its_identity() {
    let mut document = reset_fixture();
    document.keybindings = serde_json::from_value(serde_json::json!({
        "close_tab": null,
        "new_workspace": "shift-cmd-t"
    }))
    .unwrap();
    document.revision = 17;
    let schema_version = document.schema_version;
    assert!(
        !document.terminal_themes.is_empty(),
        "the fixture should install themes"
    );

    document.reset_all();

    assert_eq!(
        document.preferences,
        AppearancePreferences::default(),
        "every preference should return to its default"
    );
    assert!(
        document.terminal_themes.is_empty(),
        "the imported catalog should empty"
    );
    assert_eq!(document.keybindings, Default::default());
    assert_eq!(
        document.revision, 17,
        "the write order should carry forward"
    );
    assert_eq!(document.schema_version, schema_version);
    document.validate().unwrap();
}

/// A selection naming an imported theme is valid only while that theme is installed, so
/// emptying the catalog and defaulting the preferences have to land in the same edit.
#[test]
fn resetting_the_whole_document_releases_a_selected_imported_theme() {
    let mut document = reset_fixture();
    let imported = document.terminal_themes[0].id.clone();
    document.preferences.terminal.themes.dark = imported.clone();
    document.validate().unwrap();

    document.reset_all();

    assert_ne!(document.preferences.terminal.themes.dark, imported);
    document.validate().unwrap();
}

#[test]
fn every_color_role_can_be_removed_without_changing_other_overrides() {
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
    let terminal_id = ThemeId::builtin("builtin.spaceterm.light");

    for role in terminal_roles {
        let mut actual = reset_fixture();
        let retained_themes = actual.terminal_themes.clone();
        let before_window = actual.preferences.window.clone();
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
            actual.preferences.window, before_window,
            "terminal role {role}"
        );
        assert_eq!(
            actual.terminal_themes, retained_themes,
            "terminal role {role}"
        );
    }

    assert!(ResetTarget::terminal_color_override(terminal_id, "not_a_role").is_none());

    let mut sparse = SettingsDocument::default();
    let terminal_id = ThemeId::builtin("builtin.spaceterm.dark");
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

/// A Settings file that selects an uninstalled theme reads with that slot on the built-in theme
/// for its appearance, and the other slot keeps its choice.
#[test]
fn reading_a_missing_theme_selection_returns_the_slot_to_the_builtin_theme() {
    let mut document = SettingsDocument::default();
    document.terminal_themes.push(TerminalTheme {
        id: ThemeId::new("custom.kept-dark").unwrap(),
        name: String::from("Kept Dark"),
        appearance: Appearance::Dark,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides::default(),
    });
    document.preferences.terminal.themes.dark = ThemeId::new("custom.kept-dark").unwrap();
    let encoded = export_settings(&document)
        .unwrap()
        .replace("builtin.spaceterm.light", "custom.removed-light");

    let read = parse_settings(encoded.as_bytes()).unwrap();

    assert_eq!(
        read.preferences.terminal.themes.light,
        ThemeId::builtin("builtin.spaceterm.light")
    );
    assert_eq!(
        read.preferences.terminal.themes.dark,
        ThemeId::new("custom.kept-dark").unwrap()
    );
}

#[test]
fn native_settings_are_canonical_strict_and_round_trip() {
    let mut document = SettingsDocument::default();
    document.preferences.mode = AppearanceMode::Auto;
    document.preferences.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.dark");
    assert!(
        export_settings(&document).is_err(),
        "a slot should name a theme of its own appearance"
    );
    document.preferences.terminal.themes.light = ThemeId::new("missing.terminal.light").unwrap();
    assert!(
        export_settings(&document).is_err(),
        "a slot should name an installed theme"
    );
    document.preferences.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.light");
    let encoded = export_settings(&document).unwrap();
    let encoded_value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(encoded_value["schema_version"], 3);
    assert_eq!(parse_settings(encoded.as_bytes()).unwrap(), document);
    let unsupported = encoded.replacen("\"schema_version\": 3", "\"schema_version\": 2", 1);
    assert!(matches!(
        parse_settings(unsupported.as_bytes()),
        Err(SettingsDocumentError::UnsupportedVersion)
    ));
    assert!(matches!(parse_settings(br#"{"schema_version":3,"schema_version":3,"revision":0,"preferences":{},"terminal_themes":[]}"#),
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
    document.preferences.terminal.typography.base_size = f32::NAN;
    assert!(matches!(
        export_settings(&document),
        Err(SettingsDocumentError::InvalidPreferences)
    ));

    let custom = TerminalTheme {
        id: ThemeId::new("custom.alpha").unwrap(),
        name: String::from("Alpha"),
        appearance: Appearance::Dark,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides {
            foreground: Some(Color::rgba(0xffffff80)),
            ..Default::default()
        },
    };
    let document = SettingsDocument {
        terminal_themes: vec![custom],
        ..SettingsDocument::default()
    };
    assert!(matches!(
        export_settings(&document),
        Err(SettingsDocumentError::InvalidCatalog)
    ));
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
fn update_preferences_round_trip_and_reset_with_the_settings_document() {
    use crate::updates::policy::{CheckInterval, ReminderInterval, UpdatePreferences};
    let document = SettingsDocument {
        updates: UpdatePreferences {
            automatic_downloads: false,
            check_interval: CheckInterval::Hourly,
            reminder_interval: ReminderInterval::EightHours,
        },
        ..Default::default()
    };
    let encoded = export_settings(&document).unwrap();
    let mut restored = parse_settings(encoded.as_bytes()).unwrap();
    assert_eq!(restored.updates, document.updates);
    restored.reset_all();
    assert_eq!(restored.updates, UpdatePreferences::default());
}

#[test]
fn keybinding_overrides_round_trip_as_a_sparse_map() {
    let document = SettingsDocument {
        keybindings: serde_json::from_value(serde_json::json!({
            "close_tab": null,
            "new_workspace": "shift-cmd-t"
        }))
        .unwrap(),
        ..Default::default()
    };

    let encoded = export_settings(&document).unwrap();
    let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        value["keybindings"],
        serde_json::json!({"close_tab": null, "new_workspace": "shift-cmd-t"})
    );
    assert_eq!(parse_settings(encoded.as_bytes()).unwrap(), document);
    assert!(encoded.find("\"updates\"").unwrap() < encoded.find("\"keybindings\"").unwrap());
    assert!(encoded.find("\"keybindings\"").unwrap() < encoded.find("\"preferences\"").unwrap());

    let mut without_overrides = value;
    without_overrides
        .as_object_mut()
        .unwrap()
        .remove("keybindings");
    assert_eq!(
        parse_settings(&serde_json::to_vec(&without_overrides).unwrap())
            .unwrap()
            .keybindings,
        Default::default()
    );
}

#[test]
fn invalid_keybinding_overrides_are_rejected_by_the_settings_document() {
    let mut value = serde_json::to_value(SettingsDocument::default()).unwrap();
    value["keybindings"] = serde_json::json!({
        "close_tab": "shift-cmd-t",
        "new_workspace": "shift-cmd-t"
    });
    let bytes = serde_json::to_vec(&value).unwrap();
    assert_eq!(
        parse_settings(&bytes),
        Err(SettingsDocumentError::InvalidKeybindings)
    );
    let invalid: SettingsDocument = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        export_settings(&invalid),
        Err(SettingsDocumentError::InvalidKeybindings)
    );

    value["keybindings"] = serde_json::json!({"unknown_command": "shift-cmd-t"});
    assert_eq!(
        parse_settings(&serde_json::to_vec(&value).unwrap()),
        Err(SettingsDocumentError::InvalidJson)
    );

    value["keybindings"] = serde_json::json!({"close_tab": "ctrl-c"});
    let parsed = parse_settings(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(parsed).unwrap()["keybindings"],
        value["keybindings"]
    );
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

#[test]
fn replacing_settings_takes_the_imported_catalog_and_keeps_identity() {
    let imported = reset_fixture();
    let mut document = SettingsDocument {
        revision: 4,
        ..SettingsDocument::default()
    };

    document.replace_settings(imported.clone());

    assert_eq!(document.preferences, imported.preferences);
    assert_eq!(document.terminal_themes, imported.terminal_themes);
    assert_eq!(document.revision, 4);
    document.validate().unwrap();
}

#[test]
fn clipboard_settings_round_trip_import_and_reset() {
    let mut document = SettingsDocument::default();
    document.clipboard.allow_write = false;
    document.clipboard.allow_read = true;
    let restored = parse_settings(export_settings(&document).unwrap().as_bytes()).unwrap();
    assert_eq!(restored.clipboard, document.clipboard);
    let mut imported = SettingsDocument::default();
    imported.replace_settings(restored);
    assert_eq!(imported.clipboard, document.clipboard);
    imported.reset_all();
    assert_eq!(imported.clipboard, Default::default());
}
