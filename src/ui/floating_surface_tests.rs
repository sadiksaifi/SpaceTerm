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
            SystemAppearance::available(appearance).with_composition(
                crate::appearance::CompositionCapabilities::new(supported, true),
            ),
            &AvailableFonts::default(),
        )
        .expect("valid built-in surface case should resolve");
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    (resolved, prepared)
}

#[test]
fn native_window_support_does_not_disable_floating_translucency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, supported) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let (_, unsupported) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, false);

        for role in FLOATING_ROLES {
            let expected = supported.floating_surfaces().shell(role).material();
            assert!(
                expected.a < 1.0,
                "{appearance:?} {role:?} fixture must exercise translucency"
            );
            assert_eq!(
                unsupported.floating_surfaces().shell(role).material(),
                expected,
                "{appearance:?} {role:?} floating presentation must not depend on native-window support"
            );
        }
    }
}

#[test]
fn floating_material_tracks_transparency_and_blur_only_changes_filter() {
    let default_transparency = AppearancePreferences::default().background.transparency;
    for appearance in [Appearance::Light, Appearance::Dark] {
        for density in [ChromeDensity::Compact, ChromeDensity::Comfortable] {
            let (opaque, opaque_prepared) = resolve_case(appearance, density, 0.0, false, true);
            for transparency in [0.0, default_transparency, 1.0] {
                for supported in [false, true] {
                    let (plain, plain_prepared) =
                        resolve_case(appearance, density, transparency, false, supported);
                    for blur in [false, true] {
                        let (resolved, prepared) =
                            resolve_case(appearance, density, transparency, blur, supported);
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
                        for role in FLOATING_ROLES {
                            let shell = prepared.floating_surfaces().shell(role);
                            let plain_shell = plain_prepared.floating_surfaces().shell(role);
                            assert_eq!(shell.material(), plain_shell.material());
                            assert_eq!(
                                shell.backdrop_blur_radius() > gpui::px(0.0),
                                blur && transparency > 0.0
                            );
                        }
                        if transparency == 0.0 {
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
                        } else if supported {
                            assert_eq!(resolved.chrome.composition.effective, requested);
                        } else {
                            assert_eq!(
                                resolved.chrome.composition.effective,
                                WindowBackgroundAppearance::Opaque
                            );
                            assert!(resolved.chrome.composition.materials.is_opaque());
                            assert!(!resolved.chrome.composition.floating_materials.is_opaque());
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn maximum_floating_transparency_keeps_a_visible_tint_and_opaque_content() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        for role in FLOATING_ROLES {
            let alpha = prepared.floating_surfaces().shell(role).material().a;
            assert!(
                (0.65..=0.75).contains(&alpha),
                "{appearance:?} {role:?} must retain a visible tint, got {alpha}"
            );
        }
        assert_eq!(prepared.colors.text.a, 255);
        assert_eq!(prepared.colors.text_muted.a, 255);
        assert_eq!(prepared.floating_colors.input_placeholder.a, 255);
    }
}

#[test]
fn floating_text_stays_readable_over_extreme_content_at_maximum_transparency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let host = &prepared.floating_colors;
        for role in FLOATING_ROLES {
            let fill = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            let text = if role == FloatingRole::Readout {
                host.preview_foreground
            } else {
                host.text
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
        let host = &prepared.floating_colors;
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
                let foreground = host.text_muted.source_over(background);
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
        let host = &prepared.floating_colors;
        for role in [FloatingRole::Command, FloatingRole::Popover] {
            let fill = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = fill.source_over(underlay);
                let contrast = host
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
fn midgray_custom_floating_material_moves_only_its_tint_to_admit_readable_content() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (mut resolved, _) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
        let midgray = Color::rgb(0x808080);
        authored.elevated_surface_background = midgray;
        authored.preview_background = midgray;
        let prepared = ChromeAppearance::prepare(&resolved.chrome);
        let host = &prepared.floating_colors;

        assert_eq!(prepared.colors.elevated_surface_background, midgray);
        assert_eq!(prepared.colors.preview_background, midgray);
        for role in FLOATING_ROLES {
            let material = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            assert!((0.65..=0.75).contains(&(f32::from(material.a) / 255.0)));
            assert_ne!(
                (material.r, material.g, material.b),
                (midgray.r, midgray.g, midgray.b),
                "{appearance:?} {role:?} fixture must exercise the material fallback"
            );
            let foreground = if role == FloatingRole::Readout {
                host.preview_foreground
            } else {
                host.text
            };
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = material.source_over(underlay);
                assert!(
                    foreground
                        .source_over(background)
                        .contrast_ratio(background)
                        >= 4.5,
                    "{appearance:?} {role:?} custom material must admit readable content over {underlay:?}"
                );
            }
        }
    }
}

#[test]
fn floating_row_content_is_readable_on_idle_hovered_and_selected_backgrounds() {
    use super::control_theme_catalog::OverlayRow;

    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let reference = prepared.floating_colors.clone();
        let material = Color::rgba(u32::from(
            prepared
                .floating_surfaces()
                .shell(FloatingRole::Popover)
                .material(),
        ));
        let mut paint = reference.clone();
        paint.elevated_surface_background = material;
        for pick in [
            |c: &crate::appearance::ChromeColors| {
                [
                    c.elevated_surface_background,
                    c.row_foreground,
                    c.row_secondary,
                    c.row_icon,
                    c.row_match,
                    c.row_border,
                ]
            },
            |c: &crate::appearance::ChromeColors| {
                [
                    c.row_hover_background,
                    c.row_hover_foreground,
                    c.row_hover_secondary,
                    c.row_hover_icon,
                    c.row_hover_match,
                    c.row_hover_border,
                ]
            },
            |c: &crate::appearance::ChromeColors| {
                [
                    c.row_selected_background,
                    c.row_selected_foreground,
                    c.row_selected_secondary,
                    c.row_selected_icon,
                    c.row_selected_match,
                    c.row_selected_border,
                ]
            },
            |c: &crate::appearance::ChromeColors| {
                [
                    c.row_selected_hover_background,
                    c.row_selected_hover_foreground,
                    c.row_selected_hover_secondary,
                    c.row_selected_hover_icon,
                    c.row_selected_hover_match,
                    c.row_selected_hover_border,
                ]
            },
        ] as [fn(&crate::appearance::ChromeColors) -> [Color; 6]; 4]
        {
            let [fill, foreground, secondary, icon, matched, border] = pick(&reference);
            let row = OverlayRow::resolve(
                (fill, reference.elevated_surface_background),
                (pick(&paint)[0], paint.elevated_surface_background),
                [foreground, secondary, icon, matched],
                border,
            );
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = row.fill.source_over(material.source_over(underlay));
                for content in row.content {
                    assert!(
                        content.contrast_ratio(background) >= 4.5,
                        "{appearance:?} row content {content:?} must read over {background:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn custom_midgray_idle_row_uses_the_resolved_material_as_its_contrast_reference() {
    use super::control_theme_catalog::OverlayRow;

    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    std::sync::Arc::make_mut(&mut resolved.chrome)
        .colors
        .elevated_surface_background = Color::rgb(0x808080);
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let reference = &prepared.floating_colors;
    let material = Color::rgba(u32::from(
        prepared
            .floating_surfaces()
            .shell(FloatingRole::Popover)
            .material(),
    ));
    assert_ne!(reference.elevated_surface_background, material);
    let mut contrast_reference = reference.clone();
    contrast_reference.elevated_surface_background = material.with_alpha(255);
    let idle = OverlayRow::resolve(
        (
            contrast_reference.elevated_surface_background,
            contrast_reference.elevated_surface_background,
        ),
        (material, material),
        [
            contrast_reference.row_foreground,
            contrast_reference.row_secondary,
            contrast_reference.row_icon,
            contrast_reference.row_match,
        ],
        contrast_reference.row_border,
    );
    assert_eq!(idle.fill.a, 0, "the idle row inherits its shell material");
    for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
        let background = idle.fill.source_over(material.source_over(underlay));
        for content in idle.content {
            assert!(
                content.contrast_ratio(background) >= 4.5,
                "idle content {content:?} must read over resolved custom material {background:?}"
            );
        }
    }
}

#[test]
fn floating_standard_and_bare_inputs_resolve_against_their_actual_backgrounds() {
    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
    authored.elevated_surface_background = Color::rgb(0x808080);
    authored.input_background = Color::rgb(0xffffff);
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let bare = &prepared.floating_colors;
    let standard = &prepared.floating_field_colors;
    let material = Color::rgba(u32::from(
        prepared
            .floating_surfaces()
            .shell(FloatingRole::Popover)
            .material(),
    ));

    for foreground in [bare.input_text, bare.input_placeholder] {
        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let background = material.source_over(underlay);
            assert!(foreground.contrast_ratio(background) >= 4.5);
        }
    }
    for foreground in [standard.input_text, standard.input_placeholder] {
        assert!(foreground.contrast_ratio(standard.input_background) >= 4.5);
    }
    assert_ne!(bare.input_text, standard.input_text);
    assert_ne!(
        super::text_input_theme::theme(bare),
        super::text_input_theme::themed(standard, bare),
        "floating Standard and Bare variants must keep distinct foregrounds"
    );
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
        let expected_material = prepared.floating_surface(expected_host);
        let repeated_material = prepared.floating_surface(doubled);
        assert_ne!(
            expected_material, repeated_material,
            "material adjustment must not hide repeated authored composition"
        );
        let reference = gpui::rgba(expected_material.rgba_hex());
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
