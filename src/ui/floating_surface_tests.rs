use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeDensity, Color, ResolvedAppearance, SchemeCatalog, SystemAppearance,
    WindowBackgroundAppearance,
};
use crate::ui::appearance::ChromeAppearance;
use spaceterm_ui::FloatingRole;

const FLOATING_ROLES: [FloatingRole; 6] = [
    FloatingRole::Popover,
    FloatingRole::Command,
    FloatingRole::Modal,
    FloatingRole::Tooltip,
    FloatingRole::Notice,
    FloatingRole::Readout,
];

fn resolve_case(
    appearance: Appearance,
    density: ChromeDensity,
    transparency: f32,
    blur: bool,
    supported: bool,
) -> (ResolvedAppearance, ChromeAppearance) {
    let mut preferences = AppearancePreferences {
        mode: match appearance {
            Appearance::Light => AppearanceMode::Light,
            Appearance::Dark => AppearanceMode::Dark,
        },
        ..AppearancePreferences::default()
    };
    preferences.chrome.density = density;
    preferences.background.transparency = transparency;
    preferences.background.blur = blur;
    let resolved = SchemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(appearance).with_transparency(supported),
            &AvailableFonts::default(),
        )
        .expect("valid built-in surface case should resolve");
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    (resolved, prepared)
}

fn controls(prepared: &ChromeAppearance) -> spaceterm_ui::ControlThemeCatalog {
    super::control_theme_catalog::catalog(prepared, spaceterm_ui::ProgressMotion::Standard)
}

#[test]
fn floating_presentation_keeps_native_blur_independent_and_falls_back_to_opaque() {
    let default_transparency = AppearancePreferences::default().background.transparency;
    for appearance in [Appearance::Light, Appearance::Dark] {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let (opaque, opaque_prepared) = resolve_case(appearance, density, 0.0, false, true);
            let opaque_catalog = controls(&opaque_prepared);
            for transparency in [0.0, default_transparency, 1.0] {
                for supported in [false, true] {
                    let (plain, plain_prepared) =
                        resolve_case(appearance, density, transparency, false, supported);
                    let plain_catalog = controls(&plain_prepared);
                    for blur in [false, true] {
                        let (resolved, prepared) =
                            resolve_case(appearance, density, transparency, blur, supported);
                        let catalog = controls(&prepared);
                        assert_eq!(
                            catalog, plain_catalog,
                            "native blur must not restyle GPUI surfaces"
                        );
                        assert_eq!(resolved.terminal, opaque.terminal);
                        assert_eq!(resolved.chrome.colors, opaque.chrome.colors);
                        assert_eq!(prepared.colors.text, opaque_prepared.colors.text);
                        assert_eq!(prepared.colors.icon, opaque_prepared.colors.icon);
                        assert_eq!(
                            resolved.chrome.composition.materials,
                            plain.chrome.composition.materials
                        );
                        let requested = if blur {
                            WindowBackgroundAppearance::Blurred
                        } else {
                            WindowBackgroundAppearance::Transparent
                        };
                        assert_eq!(resolved.chrome.composition.requested, requested);
                        if !supported || transparency == 0.0 {
                            assert_eq!(catalog, opaque_catalog);
                            assert_eq!(
                                resolved.chrome.composition.effective,
                                WindowBackgroundAppearance::Opaque
                            );
                            for role in FLOATING_ROLES {
                                assert_eq!(
                                    prepared.floating_surfaces().shell(role).material().a,
                                    1.0,
                                    "{role:?} must honor the opaque fallback"
                                );
                            }
                        } else {
                            assert_eq!(resolved.chrome.composition.effective, requested);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn floating_text_stays_readable_over_extreme_content_at_maximum_transparency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        for role in FLOATING_ROLES {
            let fill = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            let text = if role == FloatingRole::Readout {
                prepared.colors.preview_foreground
            } else {
                prepared.colors.text
            };
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = fill.source_over(underlay);
                let foreground = text.source_over(background);
                let contrast = foreground.contrast_ratio(background);
                assert!(
                    contrast >= 4.5,
                    "{appearance:?} {role:?} ordinary text must stay readable over {underlay:?}: \
                     contrast={contrast:.2}, fill={fill:?}"
                );
            }
        }
    }
}

#[test]
fn floating_supporting_text_stays_readable_over_extreme_content_at_maximum_transparency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        // Tooltip shortcuts, modal body copy, and Find result counts use this register directly.
        // Menu and palette rows resolve their own content contrast and do not use this contract.
        for role in [
            FloatingRole::Tooltip,
            FloatingRole::Modal,
            FloatingRole::Notice,
        ] {
            let fill = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = fill.source_over(underlay);
                let foreground = prepared.colors.text_muted.source_over(background);
                let contrast = foreground.contrast_ratio(background);
                assert!(
                    contrast >= 4.5,
                    "{appearance:?} {role:?} supporting text must stay readable over {underlay:?}: \
                     contrast={contrast:.2}, fill={fill:?}"
                );
            }
        }
    }
}

#[test]
fn floating_bare_editor_placeholders_stay_readable_over_extreme_content() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        for role in [FloatingRole::Command, FloatingRole::Popover] {
            let fill = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = fill.source_over(underlay);
                let contrast = prepared
                    .floating_colors
                    .input_placeholder
                    .source_over(background)
                    .contrast_ratio(background);
                assert!(
                    contrast >= 4.5,
                    "{appearance:?} {role:?} bare editor placeholder contrast={contrast:.2} over {underlay:?}"
                );
            }
        }
    }
}

#[test]
fn translucent_authored_elevation_is_composited_once_for_shell_and_host_reference() {
    for transparency in [0.0, 0.35, 1.0] {
        let (mut resolved, _) = resolve_case(
            Appearance::Dark,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
        authored.background = Color::rgb(0x182430);
        authored.elevated_surface_background = Color::rgba(0xc0e0ff80);
        let expected_host = authored
            .elevated_surface_background
            .source_over(authored.background);
        let doubled = authored
            .elevated_surface_background
            .source_over(expected_host);

        let prepared = ChromeAppearance::prepare(&resolved.chrome);

        assert_ne!(
            expected_host, doubled,
            "fixture must distinguish repeated composition"
        );
        assert_eq!(
            prepared.floating_colors.elevated_surface_background,
            expected_host
        );
        assert_eq!(prepared.floating_colors.background, expected_host);
        let reference = gpui::rgba(expected_host.rgba_hex());
        for role in FLOATING_ROLES {
            if role == FloatingRole::Readout {
                continue;
            }
            let material = prepared.floating_surfaces().shell(role).material();
            assert_eq!(
                (material.r, material.g, material.b),
                (reference.r, reference.g, reference.b),
                "{role:?} material must share the once-composited host contrast reference"
            );
        }
    }
}

#[test]
fn floating_fields_resolve_authored_alpha_against_the_host_before_root_flattening() {
    let (mut resolved, _) =
        resolve_case(Appearance::Dark, ChromeDensity::Compact, 0.35, true, true);
    let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
    authored.background = Color::rgb(0x101010);
    authored.elevated_surface_background = Color::rgb(0xe0e0e0);
    authored.panel_background = Color::rgba(0);
    authored.input_background = Color::rgba(0x00000080);
    let expected = authored
        .input_background
        .source_over(authored.elevated_surface_background);

    let prepared = ChromeAppearance::prepare(&resolved.chrome);

    assert_eq!(prepared.floating_colors.input_background, expected);
    assert_ne!(
        prepared.floating_colors.input_background,
        prepared.colors.input_background
    );
}
