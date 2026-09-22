use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeColorOverrides, ChromeDensity, ChromeScheme, Color, ColorProvenance,
    CompositionCapabilities, CustomScheme, ResolvedAppearance, SchemeCatalog, SchemeId,
    SchemeMetadata, SystemAppearance, WindowBackgroundAppearance,
};
use crate::ui::appearance::{ChromeAppearance, DisabledControlDiagnostic, FloatingControlFamily};
use spaceterm_ui::{FloatingRole, FloatingShell};

const FLOATING_ROLES: [FloatingRole; 6] = [
    FloatingRole::Popover,
    FloatingRole::Command,
    FloatingRole::Modal,
    FloatingRole::Tooltip,
    FloatingRole::Notice,
    FloatingRole::Readout,
];

#[test]
fn light_selections_and_terminal_share_the_common_surface() {
    let (resolved, prepared) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 0.0, true, true);
    let selected = Color::rgb(0xfafafa);
    let popup_selected = selected;
    for (role, fill) in [
        ("Tab", prepared.colors.tab_active_background),
        (
            "sidebar",
            prepared.panel_controls.reference.row_selected_background,
        ),
        (
            "unfocused sidebar",
            prepared
                .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel)
                .row_selected_background,
        ),
        (
            "window segment",
            prepared.segmented_control_colors.selection_background,
        ),
        (
            "panel segment",
            prepared.panel_controls.segmented.selection_background,
        ),
        (
            "card segment",
            prepared.card_controls.segmented.selection_background,
        ),
        (
            "floating segment",
            prepared.floating_segmented_colors.selection_background,
        ),
        ("Terminal", resolved.terminal.colors.background),
    ] {
        assert_eq!(
            fill, selected,
            "Light {role} should share the selected surface"
        );
    }
    for (role, fill) in [
        (
            "unfocused popup",
            prepared
                .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
                .row_selected_background,
        ),
        (
            "menu",
            prepared.floating_control_colors.row_selected_background,
        ),
    ] {
        assert_eq!(
            fill, popup_selected,
            "Light {role} should use the common raised selection surface"
        );
    }
    // Menu, ComboBox and Command Palette rows share this final paint resolver. Its contrast
    // policy must preserve the selected color, not only the prepared role inspected above.
    let reference = &prepared.floating_colors;
    let mut popup = reference.clone();
    popup.elevated_surface_background =
        prepared.floating_surface(prepared.floating_colors.elevated_surface_background);
    let rows = super::control_theme_catalog::overlay_list_rows_with_policy(
        reference,
        &popup,
        super::control_theme_catalog::OverlayRowPolicy::prepared(&prepared),
    );
    let expected = [
        popup_selected,
        reference.row_selected_foreground,
        reference.row_selected_secondary,
        reference.row_selected_icon,
        reference.row_selected_match,
        reference.row_selected_border,
    ]
    .map(|color| gpui::rgba(color.rgba_hex()));
    assert_eq!(
        rows.resolve(true, true, false),
        spaceterm_ui::ListRowPaint::new(
            expected[0],
            expected[1],
            expected[2],
            expected[3],
            expected[4],
            expected[5],
        ),
        "Light popup rows must paint raised selection over the base surface"
    );
}

#[test]
fn light_navigation_keeps_quiet_edges_and_uses_the_rim_for_selected_hover() {
    for transparency in [0.0, 0.35, 1.0] {
        let (_, prepared) = resolve_case(
            Appearance::Light,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        let colors = &prepared.colors;
        for (name, fill, hover, rim, hover_rim) in [
            (
                "Tab",
                colors.tab_active_background,
                colors.tab_active_hover_background,
                colors.tab_active_border,
                colors.tab_active_border,
            ),
            (
                "Workspace row",
                colors.row_selected_background,
                colors.row_selected_hover_background,
                colors.row_selected_border,
                colors.row_selected_hover_border,
            ),
        ] {
            let paint = super::selection_chip::ChipPaint {
                fill: Some(fill),
                hover_fill: Some(hover),
                rim: Some(rim),
                hover_rim: Some(hover_rim),
            }
            .selected_on(&prepared, colors.background);
            let host = paint.fill.unwrap().source_over(colors.background);
            let edge = paint.rim.unwrap().source_over(host);
            let hovered_edge = paint.hover_rim.unwrap().source_over(host);
            assert_eq!(
                paint.fill, paint.hover_fill,
                "Light {name} at {transparency}: selected hover should keep its content fill"
            );
            assert!(
                (1.15..=1.4).contains(&edge.contrast_ratio(host)),
                "Light {name} at {transparency}: resting edge {edge:?} must remain visible and quiet on {host:?}"
            );
            assert!(
                hovered_edge.contrast_ratio(host) > edge.contrast_ratio(host),
                "Light {name} at {transparency}: selected hover must strengthen its rim"
            );
        }
    }
}

#[test]
fn light_settings_grouping_uses_surface_separation_and_quiet_outer_edges() {
    use super::appearance::settings::SettingsSurfaceRole::{Canvas, Card};

    for transparency in [0.0, 0.35, 1.0] {
        let (resolved, _) = resolve_case(
            Appearance::Light,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings, _) =
            super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
        let canvas = settings.surface(Canvas).background;
        let card = settings.surface(Card).background;
        let edge = settings.card_edge().source_over(card);
        let divider = settings.separator(Card).source_over(card);

        assert!(
            card.r < canvas.r && card.contrast_ratio(canvas) >= 1.05,
            "Light Settings at {transparency}: card {card:?} must separate from canvas {canvas:?} without relying on an outline"
        );
        assert!(
            edge == card,
            "Light Settings at {transparency}: a group must not need a decorative outline"
        );
        assert!(
            (1.12..=1.22).contains(&divider.contrast_ratio(card)),
            "Light Settings at {transparency}: inset row rule {divider:?} must remain visible and quiet on {card:?}"
        );
    }
}

#[test]
fn dark_search_fields_separate_from_their_host_without_competing_with_selection() {
    for transparency in [0.0, 0.35] {
        let (resolved, _) = resolve_case(
            Appearance::Dark,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings, _) =
            super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
        let host = settings
            .chrome
            .control_host_background(spaceterm_ui::ControlHost::Panel);
        let colors = &settings.chrome.panel_controls.colors;
        let field = colors.input_background.source_over(host);
        assert!(
            field.r > host.r && field.contrast_ratio(host) >= 1.12,
            "Dark search fill must remain distinct at {transparency}: {field:?} on {host:?}"
        );
        assert!(colors.input_text.source_over(field).contrast_ratio(field) >= 4.5);
        assert!(
            colors
                .input_placeholder
                .source_over(field)
                .contrast_ratio(field)
                >= 3.0
        );
        let selection = settings
            .chrome
            .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel);
        let selected = settings
            .chrome
            .selection_surface(
                settings.chrome.colors.panel_background,
                selection.row_selected_background,
            )
            .source_over(host);
        assert!(
            selected.r > field.r,
            "a search field must not resemble the current section"
        );
    }
}

#[test]
fn dark_settings_sidebar_separates_from_canvas_without_changing_transmission() {
    use super::appearance::settings::SettingsSurfaceRole::{Canvas, Sidebar};

    for transparency in [0.0, 0.35] {
        for blur in [false, true] {
            let (resolved, _) = resolve_case(
                Appearance::Dark,
                ChromeDensity::Compact,
                transparency,
                blur,
                true,
            );
            let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
            let (active, inactive) =
                super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
            for settings in [active, inactive] {
                let sidebar = settings.surface(Sidebar);
                let canvas = settings.surface(Canvas);
                assert!(
                    sidebar.background.r > canvas.background.r
                        && sidebar.background.contrast_ratio(canvas.background) >= 1.15,
                    "Dark Settings sidebar must separate by fill at {transparency}: {sidebar:?}, {canvas:?}"
                );
                assert_eq!(sidebar.paint.a, canvas.paint.a);
                assert!(settings.sidebar_edge().is_none());
                let colors = settings
                    .chrome
                    .host_colors(spaceterm_ui::ControlHost::Panel);
                assert_eq!(
                    colors.row_background, colors.panel_background,
                    "resting navigation rows must not become dark tiles on the lighter sidebar"
                );
                if settings.chrome.active {
                    let hover = settings
                        .chrome
                        .materials
                        .paint(
                            crate::appearance::SurfaceRole::Surface,
                            sidebar.semantic,
                            colors.row_hover_background,
                        )
                        .source_over(sidebar.background);
                    let selected_colors = settings
                        .chrome
                        .unfocused_selection_colors(spaceterm_ui::ControlHost::Panel);
                    let selected = settings
                        .chrome
                        .selection_surface(
                            sidebar.semantic,
                            selected_colors.row_selected_background,
                        )
                        .source_over(sidebar.background);
                    assert!(hover.r > sidebar.background.r);
                    assert!(
                        selected.r > hover.r,
                        "selection must remain distinct from hover"
                    );
                    assert!(selected.contrast_ratio(sidebar.background) >= 1.15);
                }
            }
        }
    }
}

#[test]
fn dark_settings_sidebar_preserves_an_explicit_panel_override() {
    use super::appearance::settings::SettingsSurfaceRole::Sidebar;

    let (mut resolved, _) =
        resolve_case(Appearance::Dark, ChromeDensity::Compact, 0.35, true, true);
    let panel = Color::rgb(0x243044);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.colors.panel_background = panel;
    chrome
        .provenance
        .insert("panel_background", ColorProvenance::Overridden);
    let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
    let (settings, _) =
        super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
    assert_eq!(settings.surface(Sidebar).semantic, panel);
}

#[test]
fn settings_surfaces_follow_window_transparency_and_controls_use_their_actual_hosts() {
    use super::appearance::settings::SettingsSurfaceRole::{Canvas, Card, Sidebar};

    let cumulative_alpha = |under: u8, over: u8| {
        let under = f64::from(under) / 255.0;
        let over = f64::from(over) / 255.0;
        ((over + under * (1.0 - over)) * 255.0).round() as u8
    };
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            let (resolved, _) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let authored = resolved.chrome.colors.clone();
            let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
            let (settings, _) =
                super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
            let sidebar = settings.surface(Sidebar);
            let canvas = settings.surface(Canvas);
            let card = settings.surface(Card);

            assert_eq!(resolved.chrome.colors, authored);
            for (host, expected) in [
                (spaceterm_ui::ControlHost::Panel, sidebar.background),
                (spaceterm_ui::ControlHost::Window, canvas.background),
                (spaceterm_ui::ControlHost::Card, card.background),
            ] {
                assert_eq!(settings.chrome.control_host_background(host), expected);
            }
            if transparency == 0.0 {
                assert_eq!([sidebar.paint.a, canvas.paint.a, card.paint.a], [255; 3]);
            } else {
                let card_alpha = cumulative_alpha(canvas.paint.a, card.paint.a);
                if appearance == Appearance::Dark {
                    assert_eq!(
                        canvas.paint,
                        settings.chrome.surface(
                            crate::appearance::SurfaceRole::Sheet,
                            settings.chrome.colors.background,
                        ),
                        "Settings must not halve the requested window transparency"
                    );
                    assert_eq!(sidebar.paint.a, canvas.paint.a);
                } else {
                    assert!(sidebar.paint.a < canvas.paint.a);
                }
                assert!(canvas.paint.a < card_alpha);
                assert!(card_alpha < u8::MAX);
            }
        }

        let (resolved, _) = resolve_case_with_transparency_accessibility(
            appearance,
            ChromeDensity::Compact,
            1.0,
            true,
            true,
            false,
        );
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let (settings, _) =
            super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
        assert_eq!(
            [
                settings.surface(Sidebar).paint.a,
                settings.surface(Canvas).paint.a,
                settings.surface(Card).paint.a,
            ],
            [255; 3]
        );
    }

    let (mut resolved, _) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 0.35, true, true);
    let authored_panel = Color::rgb(0x7a91b3);
    std::sync::Arc::make_mut(&mut resolved.chrome)
        .colors
        .panel_background = authored_panel;
    let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
    let (settings, _) =
        super::appearance::settings::prepare_variants(&resolved.chrome, active, inactive);
    assert_eq!(settings.surface(Sidebar).semantic, authored_panel);
}

#[test]
fn light_unfocused_navigation_and_segments_keep_translucent_raised_selections() {
    for transparency in [0.0, 0.35, 1.0] {
        let (_, prepared) = resolve_case(
            Appearance::Light,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        let panel = prepared.unfocused_selection_colors(spaceterm_ui::ControlHost::Panel);
        // An unfocused selection takes the focused fill or the scheme's dimmer authored one,
        // never a darker fill invented to reach a floor the window's material cannot hold.
        assert!(
            [Color::rgb(0xfafafa), Color::rgb(0xf4f4f4)].contains(&panel.row_selected_background),
            "unfocused sidebar at transparency {transparency}: {:?}; active {:?}; host {:?}",
            panel.row_selected_background,
            prepared.panel_controls.reference.row_selected_background,
            prepared.control_host_background(spaceterm_ui::ControlHost::Panel),
        );
        let chip = super::selection_chip::ChipPaint {
            fill: Some(panel.row_selected_background),
            hover_fill: Some(panel.row_selected_hover_background),
            rim: Some(panel.row_selected_border),
            hover_rim: Some(panel.row_selected_hover_border),
        }
        .selected_on(&prepared, panel.panel_background);
        let fill = chip.fill.expect("selected navigation paints a fill");
        assert_eq!(
            fill,
            prepared.selection_surface(panel.panel_background, panel.row_selected_background)
        );
        assert_eq!(fill.a == 255, transparency == 0.0);
        for (host, colors) in [
            (
                spaceterm_ui::ControlHost::Window,
                &prepared.segmented_control_colors,
            ),
            (
                spaceterm_ui::ControlHost::Panel,
                &prepared.panel_controls.segmented,
            ),
            (
                spaceterm_ui::ControlHost::Card,
                &prepared.card_controls.segmented,
            ),
        ] {
            let track = colors
                .element_background
                .source_over(prepared.control_host_background(host));
            let selected = colors.selection_background.source_over(track);
            assert!(
                selected.r > track.r,
                "selected segment should remain raised on {host:?} at {transparency}"
            );
            assert_eq!(
                colors.selection_background.a == 255,
                transparency == 0.0,
                "selected segment on {host:?} must respect transparency {transparency}"
            );
        }
    }
}

#[test]
fn light_navigation_uses_raised_surfaces_without_erasing_popup_selection() {
    let (_, prepared) = resolve_case(Appearance::Light, ChromeDensity::Compact, 0.0, true, true);
    let background = Color::rgb(0xe5e5e5);
    let raised = Color::rgb(0xfafafa);
    assert_eq!(prepared.colors.background, background);
    assert_eq!(prepared.colors.title_bar_background, background);
    assert_eq!(prepared.colors.panel_background, background);
    assert_eq!(prepared.colors.elevated_surface_background, raised);
    assert_eq!(prepared.colors.tab_active_background, raised);
    assert_eq!(
        prepared.panel_controls.reference.row_selected_background,
        raised
    );
    assert_eq!(prepared.floating_colors.row_selected_background, raised);
}

#[test]
fn floating_separators_remain_visible_on_both_material_endpoints() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            for increase_contrast in [false, true] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                for prepared in [&active, &inactive] {
                    for role in FLOATING_ROLES {
                        let shell = prepared.floating_surfaces().shell(role);
                        let divider = Color::rgba(u32::from(shell.divider()));
                        for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                            let host = shell_endpoint_background(shell, underlay);
                            let contrast = divider.source_over(host).contrast_ratio(host);
                            let (floor, ceiling) = match (appearance, increase_contrast) {
                                (_, true) => (3.0, None),
                                (Appearance::Light, false) => (1.12, Some(1.22)),
                                (Appearance::Dark, false) => (1.15, Some(1.35)),
                            };
                            assert!(
                                contrast >= floor,
                                "{appearance:?} {role:?} active={} transparency={transparency} increase_contrast={increase_contrast}: separator contrast {contrast} < {floor}",
                                prepared.active,
                            );
                            if let Some(ceiling) = ceiling {
                                assert!(
                                    contrast <= ceiling,
                                    "{appearance:?} separator is too strong: {contrast} > {ceiling}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn show_borders_reinforces_dark_floating_surface_edges() {
    for transparency in [0.0, 0.35, 1.0] {
        let (mut resolved, _) = resolve_case(
            Appearance::Dark,
            ChromeDensity::Compact,
            transparency,
            true,
            true,
        );
        std::sync::Arc::make_mut(&mut resolved.chrome)
            .composition
            .capabilities
            .show_borders = true;
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        for prepared in [active, inactive] {
            for role in FLOATING_ROLES {
                let shell = prepared.floating_surfaces().shell(role);
                let edge = Color::rgba(u32::from(shell.edge()));
                for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                    let host = shell_endpoint_background(shell, underlay);
                    assert!(edge.source_over(host).contrast_ratio(host) >= 3.0);
                }
            }
        }
    }
}

#[test]
fn quiet_floating_material_preserves_readability_with_stronger_diffusion() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 0.35, 1.0] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                for prepared in [&active, &inactive] {
                    for role in [FloatingRole::Tooltip, FloatingRole::Readout] {
                        let shell = prepared.floating_surfaces().shell(role);
                        if transparency > 0.0 {
                            assert_eq!(shell.backdrop_blur_radius(), gpui::px(20.0));
                            let minimum = if increase_contrast { 0.95 } else { 0.90 };
                            assert!(shell.backdrop_tone().a >= minimum);
                        } else {
                            assert_eq!(shell.material().a, 1.0);
                        }
                        let foreground = if role == FloatingRole::Readout {
                            prepared.floating_colors.preview_foreground
                        } else {
                            prepared.floating_colors.text_secondary
                        };
                        for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                            let host = shell_endpoint_background(shell, underlay);
                            let minimum = if prepared.active || increase_contrast {
                                4.5
                            } else {
                                3.0
                            };
                            assert!(foreground.source_over(host).contrast_ratio(host) >= minimum);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn settings_separators_remain_visible_on_their_final_hosts() {
    use spaceterm_ui::ControlHost;

    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            for increase_contrast in [false, true] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                for prepared in [&active, &inactive] {
                    for host in [ControlHost::Window, ControlHost::Panel, ControlHost::Card] {
                        let background = prepared.control_host_background(host);
                        let divider = prepared.separator(host);
                        let contrast = divider.source_over(background).contrast_ratio(background);
                        let (floor, ceiling) = match (appearance, increase_contrast) {
                            (_, true) => (3.0, None),
                            (Appearance::Light, false) => (1.12, Some(1.22)),
                            (Appearance::Dark, false) => (1.15, Some(1.35)),
                        };
                        assert!(
                            contrast >= floor,
                            "{appearance:?} {host:?} active={} transparency={transparency}: separator {contrast} < {floor}",
                            prepared.active,
                        );
                        if let Some(ceiling) = ceiling {
                            assert!(
                                contrast <= ceiling,
                                "{appearance:?} {host:?} separator {contrast} > {ceiling}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn prepared_unfocused_collection_pairs_reach_every_final_host_floor() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 1.0] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let opposing_custom_host = transparency == 1.0 && appearance == Appearance::Light;
                if opposing_custom_host {
                    let colors = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
                    colors.background = Color::rgb(0xf4f4f4);
                    colors.panel_background = Color::rgb(0x181818);
                    colors.elevated_surface_background = Color::rgb(0xe8e8e8);
                    colors.row_selected_background = Color::rgb(0x777777);
                    colors.row_selected_hover_background = Color::rgb(0x696969);
                    colors.row_selected_foreground = Color::rgba(0x1259b010);
                    colors.row_selected_secondary = Color::rgba(0x8a430010);
                    colors.row_selected_icon = Color::rgba(0x146b4210);
                    colors.row_selected_match = Color::rgba(0x1259b010);
                    colors.row_selected_hover_foreground = Color::rgba(0x1259b010);
                    colors.row_selected_hover_secondary = Color::rgba(0x8a430010);
                    colors.row_selected_hover_icon = Color::rgba(0x146b4210);
                    colors.row_selected_hover_match = Color::rgba(0x1259b010);
                }
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                let primary_floor = if increase_contrast { 7.0 } else { 4.5 };
                let secondary_floor = 4.5;
                let icon_floor = if increase_contrast { 4.5 } else { 3.0 };
                let selection_floor = if increase_contrast || appearance == Appearance::Dark {
                    super::appearance::SUBDUED_SELECTION_CONTRAST
                } else {
                    1.12
                };

                for host in [
                    spaceterm_ui::ControlHost::Window,
                    spaceterm_ui::ControlHost::TitleBar,
                    spaceterm_ui::ControlHost::Panel,
                    spaceterm_ui::ControlHost::Card,
                ] {
                    let active_colors = active.host_colors(host);
                    let mixed = active.unfocused_selection_colors(host);
                    let host_background = active.control_host_background(host);
                    let semantic_host = match host {
                        spaceterm_ui::ControlHost::Window => active_colors.background,
                        spaceterm_ui::ControlHost::TitleBar => active_colors.title_bar_background,
                        spaceterm_ui::ControlHost::Panel => active_colors.panel_background,
                        spaceterm_ui::ControlHost::Card => {
                            active_colors.elevated_surface_background
                        }
                        _ => unreachable!(),
                    };
                    if !opposing_custom_host {
                        let inactive_colors = inactive.host_colors(host);
                        assert_eq!(
                            mixed.row_selected_border,
                            inactive_colors.row_selected_border
                        );
                        assert_eq!(
                            mixed.row_selected_hover_border,
                            inactive_colors.row_selected_hover_border
                        );
                    }
                    let row_host = active
                        .materials
                        .paint(
                            crate::appearance::SurfaceRole::Surface,
                            semantic_host,
                            active_colors.row_background,
                        )
                        .source_over(host_background);
                    for (fill, focused_fill, primary, secondary, icon, matched) in [
                        (
                            mixed.row_selected_background,
                            active_colors.row_selected_background,
                            mixed.row_selected_foreground,
                            mixed.row_selected_secondary,
                            mixed.row_selected_icon,
                            mixed.row_selected_match,
                        ),
                        (
                            mixed.row_selected_hover_background,
                            active_colors.row_selected_hover_background,
                            mixed.row_selected_hover_foreground,
                            mixed.row_selected_hover_secondary,
                            mixed.row_selected_hover_icon,
                            mixed.row_selected_hover_match,
                        ),
                    ] {
                        let backgrounds = [
                            active
                                .selection_surface(semantic_host, fill)
                                .source_over(host_background),
                            active
                                .selection_surface(active_colors.row_background, fill)
                                .source_over(row_host),
                        ];
                        let resting_backgrounds = [
                            active
                                .materials
                                .paint(
                                    crate::appearance::SurfaceRole::Surface,
                                    semantic_host,
                                    Color::rgba(0),
                                )
                                .source_over(host_background),
                            active
                                .materials
                                .paint(
                                    crate::appearance::SurfaceRole::Surface,
                                    active_colors.row_background,
                                    Color::rgba(0),
                                )
                                .source_over(row_host),
                        ];
                        // An unfocused selection is asked for the separation the focused one
                        // actually has, which is all a translucent fill can promise, and never
                        // for more than the subdued bound.
                        let focused_backgrounds = [
                            active
                                .selection_surface(semantic_host, focused_fill)
                                .source_over(host_background),
                            active
                                .selection_surface(active_colors.row_background, focused_fill)
                                .source_over(row_host),
                        ];
                        let selection_floor = if increase_contrast {
                            selection_floor
                        } else {
                            focused_backgrounds
                                .into_iter()
                                .zip(resting_backgrounds)
                                .map(|(focused, resting)| focused.contrast_ratio(resting))
                                .fold(selection_floor, f64::min)
                        };
                        for (background, resting) in
                            backgrounds.into_iter().zip(resting_backgrounds)
                        {
                            assert!(
                                background.contrast_ratio(resting) >= selection_floor,
                                "{appearance:?}/{host:?}/IC={increase_contrast}/transparency={transparency}: unfocused selection {background:?} must remain distinct from {resting:?}"
                            );
                            for (content, floor) in [
                                (primary, primary_floor),
                                (secondary, secondary_floor),
                                (icon, icon_floor),
                                (matched, primary_floor),
                            ] {
                                assert!(
                                    content.source_over(background).contrast_ratio(background)
                                        >= floor,
                                    "{appearance:?}/{host:?}/IC={increase_contrast}/transparency={transparency}: {content:?} on {background:?}"
                                );
                            }
                        }
                    }
                }

                let shell = active.floating_surfaces().shell(FloatingRole::Popover);
                let mixed = active.unfocused_selection_colors(spaceterm_ui::ControlHost::Floating);
                if !opposing_custom_host {
                    let inactive_colors = inactive.host_colors(spaceterm_ui::ControlHost::Floating);
                    assert_eq!(
                        mixed.row_selected_border,
                        inactive_colors.row_selected_border
                    );
                    assert_eq!(
                        mixed.row_selected_hover_border,
                        inactive_colors.row_selected_hover_border
                    );
                }
                let focused_floating = active.host_colors(spaceterm_ui::ControlHost::Floating);
                for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                    let host = shell_endpoint_background(shell, underlay);
                    for (fill, focused_fill, primary, secondary, icon, matched) in [
                        (
                            mixed.row_selected_background,
                            focused_floating.row_selected_background,
                            mixed.row_selected_foreground,
                            mixed.row_selected_secondary,
                            mixed.row_selected_icon,
                            mixed.row_selected_match,
                        ),
                        (
                            mixed.row_selected_hover_background,
                            focused_floating.row_selected_hover_background,
                            mixed.row_selected_hover_foreground,
                            mixed.row_selected_hover_secondary,
                            mixed.row_selected_hover_icon,
                            mixed.row_selected_hover_match,
                        ),
                    ] {
                        let background = fill.source_over(host);
                        let selection_floor = if increase_contrast {
                            selection_floor
                        } else {
                            selection_floor.min(focused_fill.source_over(host).contrast_ratio(host))
                        };
                        assert!(
                            background.contrast_ratio(host) >= selection_floor,
                            "{appearance:?}/Floating/IC={increase_contrast}/transparency={transparency}: unfocused selection {background:?} must remain distinct from {host:?}"
                        );
                        for (content, floor) in [
                            (primary, primary_floor),
                            (secondary, secondary_floor),
                            (icon, icon_floor),
                            (matched, primary_floor),
                        ] {
                            assert!(
                                content.source_over(background).contrast_ratio(background) >= floor,
                                "{appearance:?}/Floating/IC={increase_contrast}/transparency={transparency}: {content:?} on {background:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn disabled_segmented_content_remains_readable_on_both_activity_tracks() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 0.35, 1.0] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                for prepared in [&active, &inactive] {
                    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
                    let floating_hosts = [Color::rgb(0), Color::rgb(0xffffff)]
                        .map(|underlay| shell_endpoint_background(shell, underlay));
                    for (name, paint, hosts) in [
                        (
                            "Window",
                            &prepared.segmented_control_colors,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Window);
                                2],
                        ),
                        (
                            "TitleBar",
                            &prepared.title_bar_controls.segmented,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar);
                                2],
                        ),
                        (
                            "Panel",
                            &prepared.panel_controls.segmented,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Panel); 2],
                        ),
                        (
                            "Card",
                            &prepared.card_controls.segmented,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Card); 2],
                        ),
                        (
                            "Floating",
                            &prepared.floating_segmented_colors,
                            floating_hosts,
                        ),
                    ] {
                        let minimum = if increase_contrast { 4.5 } else { 3.0 };
                        for host in hosts {
                            let track = paint.element_background.source_over(host);
                            let selected = paint.selection_disabled_background.source_over(track);
                            assert!(
                                selected.contrast_ratio(track) >= 1.12,
                                "{appearance:?}/{name} disabled selected fill on active={} track {track:?}: {:?} must retain the 1.12 selection step",
                                prepared.active,
                                paint.selection_disabled_background,
                            );
                            for (state, label, background) in [
                                ("unselected", paint.text_disabled, track),
                                ("selected", paint.selection_disabled_foreground, selected),
                            ] {
                                let contrast =
                                    label.source_over(background).contrast_ratio(background);
                                assert!(
                                    contrast >= minimum,
                                    "{appearance:?}/{name}/{state}, active={}, IC={increase_contrast}, transparency={transparency}: {contrast:.3} < {minimum}, label={label:?}, background={background:?}",
                                    prepared.active
                                );
                            }
                            if increase_contrast {
                                let border = paint.selection_disabled_border.source_over(track);
                                assert!(
                                    border.contrast_ratio(track) >= 3.0,
                                    "{appearance:?}/{name} disabled boundary on active={} track {track:?}",
                                    prepared.active
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn final_disabled_control_paints_are_identical_across_window_activity() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 0.35, 1.0] {
                let mut preferences = AppearancePreferences {
                    mode: if appearance == Appearance::Light {
                        AppearanceMode::Light
                    } else {
                        AppearanceMode::Dark
                    },
                    ..AppearancePreferences::default()
                };
                preferences.background.transparency = transparency;
                let resolved = SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &preferences,
                        SystemAppearance::available(appearance).with_composition(
                            CompositionCapabilities {
                                increase_contrast,
                                ..CompositionCapabilities::new(true, true)
                            },
                        ),
                        &AvailableFonts::default(),
                    )
                    .unwrap();
                let active = ChromeAppearance::prepare_for_activity(&resolved.chrome, true);
                let inactive = ChromeAppearance::prepare_for_activity(&resolved.chrome, false);
                for (name, active_colors, inactive_colors) in [
                    (
                        "Floating",
                        &active.floating_control_colors,
                        &inactive.floating_control_colors,
                    ),
                    (
                        "Floating segmented",
                        &active.floating_segmented_colors,
                        &inactive.floating_segmented_colors,
                    ),
                    (
                        "Floating field",
                        &active.floating_field_colors,
                        &inactive.floating_field_colors,
                    ),
                    ("Window", &active.control_colors, &inactive.control_colors),
                    (
                        "Panel",
                        &active.panel_controls.colors,
                        &inactive.panel_controls.colors,
                    ),
                    (
                        "Card",
                        &active.card_controls.colors,
                        &inactive.card_controls.colors,
                    ),
                ] {
                    macro_rules! identical { ($($field:ident),+ $(,)?) => { $(assert_eq!(active_colors.$field, inactive_colors.$field, "{appearance:?}/{name}/{}, IC={increase_contrast}, transparency={transparency}", stringify!($field));)+ }; }
                    if name == "Floating segmented" {
                        identical!(text_disabled);
                        if increase_contrast {
                            identical!(ghost_element_disabled_border);
                        }
                    } else {
                        identical!(
                            text_disabled,
                            element_disabled,
                            element_disabled_foreground,
                            element_disabled_icon,
                            element_disabled_border,
                            ghost_element_disabled,
                            ghost_element_disabled_foreground,
                            ghost_element_disabled_icon,
                            ghost_element_disabled_border,
                            toggle_off_disabled_background,
                            toggle_off_disabled_mark,
                            toggle_off_disabled_label,
                            toggle_off_disabled_border,
                            toggle_on_disabled_background,
                            toggle_on_disabled_mark,
                            toggle_on_disabled_label,
                            toggle_on_disabled_border,
                            input_disabled_background,
                            input_disabled_text,
                            input_disabled_border
                        );
                    }
                    let active_selection = (
                        active_colors.selection_disabled_background,
                        active_colors.selection_disabled_foreground,
                        active_colors.selection_disabled_icon,
                        active_colors.selection_disabled_border,
                    );
                    let inactive_selection = (
                        inactive_colors.selection_disabled_background,
                        inactive_colors.selection_disabled_foreground,
                        inactive_colors.selection_disabled_icon,
                        inactive_colors.selection_disabled_border,
                    );
                    if active_selection == inactive_selection {
                        continue;
                    }

                    assert_eq!(name, "Floating segmented");
                    assert!(active.disabled_diagnostics.contains(
                        &DisabledControlDiagnostic::SharedPaint {
                            family: FloatingControlFamily::Segmented,
                        }
                    ));
                    assert!(
                        active
                            .floating_fallbacks
                            .contains(&FloatingControlFamily::Segmented)
                    );
                    assert!(
                        inactive
                            .floating_fallbacks
                            .contains(&FloatingControlFamily::Segmented)
                    );

                    let minimum = if increase_contrast { 4.5 } else { 3.0 };
                    let endpoints = |prepared: &ChromeAppearance| {
                        let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
                        [Color::rgb(0), Color::rgb(0xffffff)]
                            .map(|underlay| shell_endpoint_background(shell, underlay))
                    };
                    let active_hosts = endpoints(&active);
                    let inactive_hosts = endpoints(&inactive);
                    let active_tracks =
                        active_hosts.map(|host| active_colors.element_background.source_over(host));
                    let inactive_tracks = inactive_hosts
                        .map(|host| inactive_colors.element_background.source_over(host));
                    for (colors, tracks) in [
                        (active_colors, active_tracks),
                        (inactive_colors, inactive_tracks),
                    ] {
                        for track in tracks {
                            let fill = colors.selection_disabled_background.source_over(track);
                            assert!(fill.contrast_ratio(track) >= 1.12);
                            assert!(
                                colors
                                    .selection_disabled_foreground
                                    .source_over(fill)
                                    .contrast_ratio(fill)
                                    >= minimum
                            );
                        }
                    }

                    let active_enabled = active_tracks
                        .map(|track| active_colors.selection_background.source_over(track));
                    let inactive_enabled = inactive_tracks
                        .map(|track| inactive_colors.selection_background.source_over(track));
                    let shared_cap = active_enabled
                        .into_iter()
                        .zip(active_tracks)
                        .chain(inactive_enabled.into_iter().zip(inactive_tracks))
                        .map(|(fill, track)| fill.contrast_ratio(track))
                        .fold(f64::INFINITY, f64::min);
                    let admits_shared = (0..=u8::MAX).any(|channel| {
                        let fill = Color::from_rgb_components(channel, channel, channel);
                        active_enabled
                            .into_iter()
                            .zip(active_tracks)
                            .chain(inactive_enabled.into_iter().zip(inactive_tracks))
                            .all(|(enabled, track)| {
                                let contrast = fill.contrast_ratio(track);
                                let direction_matches =
                                    (fill.r >= track.r) == (enabled.r >= track.r);
                                direction_matches
                                    && contrast >= 1.12
                                    && contrast <= shared_cap + 1e-9
                            })
                    });
                    assert!(
                        !admits_shared,
                        "a split is allowed only when no shared quantized chip satisfies the selected floor and cap"
                    );
                }
            }
        }
    }
}

#[test]
fn title_bar_disabled_controls_split_only_when_the_shared_fill_cap_is_infeasible() {
    let (mut resolved, _) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 0.35, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.composition.capabilities.increase_contrast = false;
    chrome.composition.capabilities.show_borders = false;
    chrome.colors.background = Color::rgb(0x202020);
    chrome.colors.title_bar_background = Color::rgb(0x101010);
    chrome.colors.title_bar_inactive_background = Color::rgb(0xf0f0f0);
    chrome.colors.element_disabled = Color::rgba(0x303030b0);
    chrome.colors.element_disabled_foreground = Color::rgb(0xffffff);
    chrome.colors.element_disabled_icon = Color::rgb(0xffffff);
    chrome.colors.element_disabled_border = Color::rgba(0);

    let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
    assert!(
        active
            .disabled_diagnostics
            .contains(&DisabledControlDiagnostic::SharedPaint {
                family: FloatingControlFamily::Element,
            })
    );
    let active_colors = &active.title_bar_controls.colors;
    let inactive_colors = &inactive.title_bar_controls.colors;
    assert_ne!(
        active_colors.element_disabled,
        inactive_colors.element_disabled
    );
    for (prepared, colors) in [(&active, active_colors), (&inactive, inactive_colors)] {
        let host = prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar);
        let ordinary = colors.element_background.source_over(host);
        let disabled = colors.element_disabled.source_over(host);
        assert!(disabled.contrast_ratio(host) <= ordinary.contrast_ratio(host) + 1e-9);
        assert!(
            colors
                .element_disabled_foreground
                .source_over(disabled)
                .contrast_ratio(disabled)
                >= 3.0
        );
    }
}

#[test]
fn inactive_prepared_control_hosts_suppress_hover_without_clearing_selection() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 0.35, 1.0] {
                let mut preferences = AppearancePreferences {
                    mode: if appearance == Appearance::Light {
                        AppearanceMode::Light
                    } else {
                        AppearanceMode::Dark
                    },
                    ..AppearancePreferences::default()
                };
                preferences.background.transparency = transparency;
                let resolved = SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &preferences,
                        SystemAppearance::available(appearance).with_composition(
                            CompositionCapabilities {
                                increase_contrast,
                                ..CompositionCapabilities::new(true, true)
                            },
                        ),
                        &AvailableFonts::default(),
                    )
                    .unwrap();
                let prepared = ChromeAppearance::prepare_for_activity(&resolved.chrome, false);
                for (name, colors) in [
                    ("Window", &prepared.control_colors),
                    ("Panel", &prepared.panel_controls.colors),
                    ("Card", &prepared.card_controls.colors),
                    ("Floating", &prepared.floating_control_colors),
                ] {
                    for (family, hover, idle) in [
                        (
                            "ordinary fill",
                            colors.element_hover,
                            colors.element_background,
                        ),
                        (
                            "ordinary text",
                            colors.element_hover_foreground,
                            colors.element_foreground,
                        ),
                        (
                            "ghost fill",
                            colors.ghost_element_hover,
                            colors.ghost_element_background,
                        ),
                        (
                            "primary fill",
                            colors.primary_hover_background,
                            colors.primary_background,
                        ),
                        (
                            "destructive fill",
                            colors.destructive_hover_background,
                            colors.destructive_background,
                        ),
                        (
                            "toggle off",
                            colors.toggle_off_hover_background,
                            colors.toggle_off_background,
                        ),
                        (
                            "toggle on",
                            colors.toggle_on_hover_background,
                            colors.toggle_on_background,
                        ),
                        (
                            "selection",
                            colors.selection_hover_background,
                            colors.selection_background,
                        ),
                    ] {
                        assert_eq!(
                            hover, idle,
                            "{appearance:?}/{name}/{family}, IC={increase_contrast}, transparency={transparency}"
                        );
                    }
                    assert_eq!(colors.focus_ring.a, 0, "{name}");
                    assert_eq!(
                        colors.selection_background, colors.element_background,
                        "{name}: inactive selection uses the ordinary resting fill"
                    );
                    assert_eq!(
                        colors.selection_border, colors.element_border,
                        "{name}: inactive selection keeps the ordinary boundary while label emphasis carries selection"
                    );
                }
            }
        }
    }
}

#[test]
fn increased_contrast_reaches_final_host_floors_without_mutating_resolved_colors() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            let mut preferences = AppearancePreferences {
                mode: match appearance {
                    Appearance::Light => AppearanceMode::Light,
                    Appearance::Dark => AppearanceMode::Dark,
                },
                ..AppearancePreferences::default()
            };
            preferences.background.transparency = transparency;
            preferences.background.blur = true;
            let resolved = SchemeCatalog::default()
                .resolve(
                    AppearanceGeneration::INITIAL,
                    &preferences,
                    SystemAppearance::available(appearance).with_composition(
                        CompositionCapabilities {
                            increase_contrast: true,
                            ..CompositionCapabilities::new(true, true)
                        },
                    ),
                    &AvailableFonts::default(),
                )
                .unwrap();
            let authored = resolved.chrome.colors.clone();
            for active in [true, false] {
                let prepared = ChromeAppearance::prepare_for_activity(&resolved.chrome, active);
                for host in [&prepared.panel_controls, &prepared.card_controls] {
                    assert!(
                        host.reference
                            .text
                            .contrast_ratio(host.reference.background)
                            >= 7.0
                    );
                    assert!(
                        host.reference
                            .text_disabled
                            .contrast_ratio(host.reference.background)
                            >= 4.5
                    );
                }
                let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
                for endpoint in [Color::rgb(0), Color::rgb(0xffffff)] {
                    let background = shell_endpoint_background(shell, endpoint);
                    let edge = Color::rgba(u32::from(shell.edge()));
                    assert!(
                        edge.source_over(background).contrast_ratio(background) >= 3.0,
                        "floating edge: {appearance:?}, transparency={transparency}, active={active}"
                    );
                    assert!(
                        prepared.floating_colors.text.contrast_ratio(background) >= 7.0,
                        "floating text: {appearance:?}, transparency={transparency}, active={active}, background={background:?}"
                    );
                    let field = prepared
                        .floating_field_colors
                        .input_background
                        .source_over(background);
                    assert!(
                        prepared
                            .floating_field_colors
                            .input_text
                            .contrast_ratio(field)
                            >= 7.0,
                        "field text: {appearance:?}, transparency={transparency}, active={active}, background={field:?}"
                    );
                }
                assert_eq!(resolved.chrome.colors, authored);
            }
        }
    }
}

#[test]
fn ordinary_contrast_sparse_custom_rows_and_tabs_read_on_their_material_hosts() {
    let scheme_id = SchemeId::new("test.opposing-app-owned-hosts").unwrap();
    let custom = CustomScheme::Chrome(Box::new(ChromeScheme {
        window_background: None,
        id: scheme_id.clone(),
        name: "Opposing App-owned Hosts".to_owned(),
        appearance: Appearance::Light,
        metadata: SchemeMetadata::default(),
        colors: ChromeColorOverrides {
            background: Some(Color::rgb(0xffffff)),
            panel_background: Some(Color::rgb(0x101010)),
            title_bar_background: Some(Color::rgb(0x101010)),
            title_bar_inactive_background: Some(Color::rgb(0x101010)),
            ..ChromeColorOverrides::default()
        },
    }));
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..AppearancePreferences::default()
    };
    preferences.chrome.schemes.light = scheme_id;
    preferences.background.transparency = 1.0;
    let resolved = SchemeCatalog::from_custom_schemes(&[custom])
        .unwrap()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light)
                .with_composition(CompositionCapabilities::new(true, true)),
            &AvailableFonts::default(),
        )
        .unwrap();
    let prepared = ChromeAppearance::prepare(&resolved.chrome);

    let panel = &prepared.panel_controls.reference;
    let panel_host = prepared.control_host_background(spaceterm_ui::ControlHost::Panel);
    let row = prepared
        .materials
        .paint(
            crate::appearance::SurfaceRole::Surface,
            panel.panel_background,
            panel.row_background,
        )
        .source_over(panel_host);
    assert!(
        panel.row_foreground.source_over(row).contrast_ratio(row) >= 4.5,
        "ordinary row content must read on its final Panel material host: content={:?}, host={row:?}",
        panel.row_foreground
    );

    let title_bar = prepared.colors.title_bar_background;
    let title_bar_host = prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar);
    for (name, fill, foreground) in [
        (
            "inactive",
            prepared.colors.tab_inactive_background,
            prepared.colors.tab_inactive_foreground,
        ),
        (
            "active",
            prepared.colors.tab_active_background,
            prepared.colors.tab_active_foreground,
        ),
    ] {
        let tab = prepared
            .materials
            .paint(crate::appearance::SurfaceRole::Surface, title_bar, fill)
            .source_over(title_bar_host);
        assert!(
            foreground.source_over(tab).contrast_ratio(tab) >= 4.5,
            "ordinary {name} Tab content must read on its final title-bar material host: content={foreground:?}, host={tab:?}"
        );
    }
}

#[test]
fn increased_contrast_app_owned_state_pairs_reach_their_final_material_hosts() {
    let (mut resolved, _) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 1.0, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.composition.capabilities.increase_contrast = true;
    let white = Color::rgb(0xffffff);
    let black = Color::rgb(0x000000);
    chrome.colors.background = white;
    chrome.colors.panel_background = white;
    chrome.colors.title_bar_background = white;
    chrome.colors.title_bar_inactive_background = white;
    for fill in [
        &mut chrome.colors.row_background,
        &mut chrome.colors.row_hover_background,
        &mut chrome.colors.row_selected_background,
        &mut chrome.colors.row_selected_hover_background,
        &mut chrome.colors.navigation_selected_background,
        &mut chrome.colors.tab_inactive_background,
        &mut chrome.colors.tab_hover_background,
        &mut chrome.colors.tab_active_background,
        &mut chrome.colors.tab_active_hover_background,
    ] {
        *fill = black;
    }
    for content in [
        &mut chrome.colors.row_foreground,
        &mut chrome.colors.row_secondary,
        &mut chrome.colors.row_icon,
        &mut chrome.colors.row_hover_foreground,
        &mut chrome.colors.row_hover_secondary,
        &mut chrome.colors.row_hover_icon,
        &mut chrome.colors.row_selected_foreground,
        &mut chrome.colors.row_selected_secondary,
        &mut chrome.colors.row_selected_icon,
        &mut chrome.colors.row_selected_hover_foreground,
        &mut chrome.colors.row_selected_hover_secondary,
        &mut chrome.colors.row_selected_hover_icon,
        &mut chrome.colors.navigation_selected_foreground,
        &mut chrome.colors.navigation_selected_secondary,
        &mut chrome.colors.navigation_selected_icon,
        &mut chrome.colors.tab_inactive_foreground,
        &mut chrome.colors.tab_inactive_icon,
        &mut chrome.colors.tab_hover_foreground,
        &mut chrome.colors.tab_hover_icon,
        &mut chrome.colors.tab_active_foreground,
        &mut chrome.colors.tab_active_icon,
        &mut chrome.colors.tab_active_hover_foreground,
        &mut chrome.colors.tab_active_hover_icon,
    ] {
        *content = white;
    }
    let authored = chrome.colors.clone();

    for active in [true, false] {
        let prepared = ChromeAppearance::prepare_for_activity(&resolved.chrome, active);
        let panel = &prepared.panel_controls.reference;
        let panel_host = prepared.control_host_background(spaceterm_ui::ControlHost::Panel);
        for (name, fill, primary, secondary, icon, selected, border) in [
            (
                "row idle",
                panel.row_background,
                panel.row_foreground,
                panel.row_secondary,
                panel.row_icon,
                false,
                None,
            ),
            (
                "row hover",
                panel.row_hover_background,
                panel.row_hover_foreground,
                panel.row_hover_secondary,
                panel.row_hover_icon,
                false,
                None,
            ),
            (
                "row selected",
                panel.row_selected_background,
                panel.row_selected_foreground,
                panel.row_selected_secondary,
                panel.row_selected_icon,
                true,
                Some(panel.row_selected_border),
            ),
            (
                "row selected hover",
                panel.row_selected_hover_background,
                panel.row_selected_hover_foreground,
                panel.row_selected_hover_secondary,
                panel.row_selected_hover_icon,
                true,
                Some(panel.row_selected_hover_border),
            ),
            (
                "navigation selected",
                panel.navigation_selected_background,
                panel.navigation_selected_foreground,
                panel.navigation_selected_secondary,
                panel.navigation_selected_icon,
                true,
                None,
            ),
        ] {
            let overlay = prepared.materials.paint(
                crate::appearance::SurfaceRole::Surface,
                panel.panel_background,
                fill,
            );
            let background = overlay.source_over(panel_host);
            assert!(
                primary.contrast_ratio(background) >= 7.0,
                "{name}, active={active}: primary={primary:?}, background={background:?}"
            );
            for (role, content) in [("secondary", secondary), ("icon", icon)] {
                assert!(
                    content.contrast_ratio(background) >= 4.5,
                    "{name}/{role}, active={active}: content={content:?}, background={background:?}"
                );
            }
            if selected {
                assert!(
                    background.contrast_ratio(panel_host) >= 1.4,
                    "{name}, active={active}: selected background={background:?}, host={panel_host:?}"
                );
            }
            if let Some(border) = border {
                let rendered_border = prepared
                    .materials
                    .edge(fill.source_over(panel.panel_background), border)
                    .source_over(panel_host);
                assert!(
                    rendered_border.contrast_ratio(panel_host) >= 3.0,
                    "{name}, active={active}: border={rendered_border:?}, host={panel_host:?}"
                );
            }
        }

        let title_bar = if active {
            prepared.colors.title_bar_background
        } else {
            prepared.colors.title_bar_inactive_background
        };
        let sheet = prepared
            .materials
            .paint(
                crate::appearance::SurfaceRole::Sheet,
                prepared.colors.background,
                prepared.colors.background,
            )
            .source_over(prepared.colors.background);
        let title_bar_host = prepared
            .materials
            .paint(
                crate::appearance::SurfaceRole::Base,
                prepared.colors.background,
                title_bar,
            )
            .source_over(sheet);
        for (name, fill, primary, icon, selected, border) in [
            (
                "tab idle",
                prepared.colors.tab_inactive_background,
                prepared.colors.tab_inactive_foreground,
                prepared.colors.tab_inactive_icon,
                false,
                None,
            ),
            (
                "tab hover",
                prepared.colors.tab_hover_background,
                prepared.colors.tab_hover_foreground,
                prepared.colors.tab_hover_icon,
                false,
                None,
            ),
            (
                "tab selected",
                prepared.colors.tab_active_background,
                prepared.colors.tab_active_foreground,
                prepared.colors.tab_active_icon,
                true,
                Some(prepared.colors.tab_active_border),
            ),
            (
                "tab selected hover",
                prepared.colors.tab_active_hover_background,
                prepared.colors.tab_active_hover_foreground,
                prepared.colors.tab_active_hover_icon,
                true,
                Some(prepared.colors.tab_active_border),
            ),
        ] {
            let overlay =
                prepared
                    .materials
                    .paint(crate::appearance::SurfaceRole::Surface, title_bar, fill);
            let background = overlay.source_over(title_bar_host);
            assert!(
                primary.contrast_ratio(background) >= 7.0,
                "{name}, active={active}: primary={primary:?}, background={background:?}"
            );
            assert!(
                icon.contrast_ratio(background) >= 4.5,
                "{name}, active={active}: icon={icon:?}, background={background:?}"
            );
            if selected {
                assert!(
                    background.contrast_ratio(title_bar_host) >= 1.4,
                    "{name}, active={active}: selected background={background:?}, host={title_bar_host:?}"
                );
            }
            if let Some(border) = border {
                let rendered_border = prepared
                    .materials
                    .edge(fill.source_over(title_bar), border)
                    .source_over(title_bar_host);
                assert!(
                    rendered_border.contrast_ratio(title_bar_host) >= 3.0,
                    "{name}, active={active}: border={rendered_border:?}, host={title_bar_host:?}"
                );
            }
        }
    }
    assert_eq!(resolved.chrome.colors, authored);
}

#[test]
fn increased_contrast_makes_custom_non_floating_material_hosts_feasible() {
    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.composition.capabilities.increase_contrast = true;
    chrome.colors.background = Color::rgb(0x606060);
    chrome.colors.panel_background = Color::rgb(0x909090);
    chrome.colors.elevated_surface_background = Color::rgb(0x909090);
    chrome.colors.title_bar_background = Color::rgb(0x909090);
    chrome.colors.title_bar_inactive_background = Color::rgb(0x909090);
    let authored = chrome.colors.clone();

    let prepared = ChromeAppearance::prepare(&resolved.chrome);

    for (name, host, colors, role, target) in [
        (
            "TitleBar",
            prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar),
            &prepared.title_bar_controls.colors,
            crate::appearance::SurfaceRole::Base,
            prepared.colors.title_bar_background,
        ),
        (
            "Panel",
            prepared.control_host_background(spaceterm_ui::ControlHost::Panel),
            &prepared.panel_controls.colors,
            crate::appearance::SurfaceRole::Base,
            prepared.colors.panel_background,
        ),
        (
            "Card",
            prepared.control_host_background(spaceterm_ui::ControlHost::Card),
            &prepared.card_controls.colors,
            crate::appearance::SurfaceRole::Surface,
            prepared.colors.elevated_surface_background,
        ),
    ] {
        assert_eq!(
            host,
            prepared
                .surface(role, target)
                .source_over(prepared.colors.background),
            "{name} guarantee must change the target consumed by the material renderer"
        );
        assert!(
            colors.text.source_over(host).contrast_ratio(host) >= 7.0,
            "{name} text must reach 7:1 on its final modeled in-window host {host:?}"
        );
        assert!(
            colors.icon.source_over(host).contrast_ratio(host) >= 7.0,
            "{name} icon must reach 7:1 on its final modeled in-window host {host:?}"
        );
    }
    assert_eq!(resolved.chrome.colors, authored);
}

#[test]
fn app_owned_content_resolves_against_custom_non_floating_material_hosts() {
    let (mut resolved, _) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 1.0, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.colors.panel_background = Color::rgb(0x202020);
    chrome.colors.elevated_surface_background = Color::rgb(0x202020);
    chrome.colors.title_bar_background = Color::rgb(0x202020);
    chrome.colors.title_bar_inactive_background = Color::rgb(0x202020);
    let white = Color::rgb(0xffffff);
    for content in [
        &mut chrome.colors.text,
        &mut chrome.colors.text_accent,
        &mut chrome.colors.text_secondary,
        &mut chrome.colors.text_muted,
        &mut chrome.colors.text_placeholder,
        &mut chrome.colors.text_disabled,
        &mut chrome.colors.icon,
        &mut chrome.colors.icon_muted,
        &mut chrome.colors.icon_disabled,
        &mut chrome.colors.link_text,
        &mut chrome.colors.link_text_hover,
        &mut chrome.colors.link_text_pressed,
        &mut chrome.colors.link_text_disabled,
    ] {
        *content = white;
    }
    let authored = chrome.colors.clone();

    let prepared = ChromeAppearance::prepare(&resolved.chrome);

    for host_role in [
        spaceterm_ui::ControlHost::Window,
        spaceterm_ui::ControlHost::TitleBar,
        spaceterm_ui::ControlHost::Panel,
        spaceterm_ui::ControlHost::Card,
    ] {
        let host = prepared.control_host_background(host_role);
        let content = prepared.host_colors(host_role);
        for (name, color, minimum) in [
            ("text", content.text, 4.5),
            ("accent text", content.text_accent, 4.5),
            ("secondary text", content.text_secondary, 4.5),
            ("muted text", content.text_muted, 4.5),
            ("placeholder text", content.text_placeholder, 4.5),
            ("disabled text", content.text_disabled, 3.0),
            ("icon", content.icon, 4.5),
            ("muted icon", content.icon_muted, 4.5),
            ("disabled icon", content.icon_disabled, 3.0),
            ("link", content.link_text, 4.5),
            ("hovered link", content.link_text_hover, 4.5),
            ("pressed link", content.link_text_pressed, 4.5),
            ("disabled link", content.link_text_disabled, 3.0),
        ] {
            assert!(
                color.source_over(host).contrast_ratio(host) >= minimum,
                "{host_role:?} app-owned {name} must reach {minimum}:1 on its final modeled in-window host {host:?}"
            );
        }
        let semantic_host = match host_role {
            spaceterm_ui::ControlHost::Window => prepared.colors.background,
            spaceterm_ui::ControlHost::TitleBar => prepared.colors.title_bar_background,
            spaceterm_ui::ControlHost::Panel => prepared.colors.panel_background,
            spaceterm_ui::ControlHost::Card => prepared.colors.elevated_surface_background,
            spaceterm_ui::ControlHost::Floating => unreachable!(),
        };
        assert_eq!(
            content.background, semantic_host,
            "app content must retain the semantic host rather than expose its materialized fill"
        );
    }
    assert_eq!(resolved.chrome.colors, authored);
}

#[test]
fn focused_selected_sidebar_row_uses_panel_prepared_focus_and_selection_roles() {
    let (mut resolved, _) =
        resolve_case(Appearance::Light, ChromeDensity::Compact, 1.0, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.composition.capabilities.increase_contrast = true;
    chrome.colors.background = Color::rgb(0xffffff);
    chrome.colors.panel_background = Color::rgb(0x202020);
    chrome.colors.sidebar_focus = Color::rgb(0xffffff);
    let authored = chrome.colors.clone();

    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let host = prepared.control_host_background(spaceterm_ui::ControlHost::Panel);
    let colors = prepared.host_colors(spaceterm_ui::ControlHost::Panel);
    let focus = colors.sidebar_focus.source_over(host);
    assert!(
        focus.contrast_ratio(host) >= 4.5,
        "sidebar focus {focus:?} must reach 4.5:1 on its final Panel host {host:?}"
    );

    let selected = prepared
        .materials
        .paint(
            crate::appearance::SurfaceRole::Surface,
            colors.panel_background,
            colors.navigation_selected_background,
        )
        .source_over(host);
    assert!(selected.contrast_ratio(host) >= 1.4);
    assert!(
        colors
            .navigation_selected_foreground
            .source_over(selected)
            .contrast_ratio(selected)
            >= 7.0
    );
    assert_eq!(resolved.chrome.colors, authored);
}

#[test]
fn accessibility_control_boundaries_reach_final_floating_endpoints() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.35, 1.0] {
            for (mode, capabilities) in [
                (
                    "Increase Contrast",
                    CompositionCapabilities {
                        increase_contrast: true,
                        ..CompositionCapabilities::new(true, true)
                    },
                ),
                (
                    "Show Borders",
                    CompositionCapabilities {
                        show_borders: true,
                        ..CompositionCapabilities::new(true, true)
                    },
                ),
            ] {
                let mut preferences = AppearancePreferences {
                    mode: if appearance == Appearance::Light {
                        AppearanceMode::Light
                    } else {
                        AppearanceMode::Dark
                    },
                    ..AppearancePreferences::default()
                };
                preferences.background.transparency = transparency;
                preferences.background.blur = true;
                let resolved = SchemeCatalog::default()
                    .resolve(
                        AppearanceGeneration::INITIAL,
                        &preferences,
                        SystemAppearance::available(appearance).with_composition(capabilities),
                        &AvailableFonts::default(),
                    )
                    .unwrap();
                let prepared = ChromeAppearance::prepare(&resolved.chrome);
                let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
                let hosts = [Color::rgb(0), Color::rgb(0xffffff)]
                    .map(|underlay| shell_endpoint_background(shell, underlay));
                let assert_host_contrast =
                    |name: &str, color: Color, hosts: [Color; 2], minimum: f64| {
                        assert_ne!(
                            color.a, 0,
                            "{appearance:?}/{mode}/{name}, transparency={transparency}: paint must be visible"
                        );
                        for host in hosts {
                            assert!(
                                color.source_over(host).contrast_ratio(host) >= minimum,
                                "{appearance:?}/{mode}/{name}, transparency={transparency}, host={host:?}, color={color:?}, minimum={minimum}"
                            );
                        }
                    };
                let colors = &prepared.floating_control_colors;
                for (name, border) in [
                    ("element", colors.element_border),
                    ("element hover", colors.element_hover_border),
                    ("element pressed", colors.element_active_border),
                    ("element disabled", colors.element_disabled_border),
                    ("ghost", colors.ghost_element_border),
                    ("ghost hover", colors.ghost_element_hover_border),
                    ("ghost pressed", colors.ghost_element_active_border),
                    ("ghost disabled", colors.ghost_element_disabled_border),
                    ("toggle off", colors.toggle_off_border),
                    ("toggle off hover", colors.toggle_off_hover_border),
                    ("toggle off pressed", colors.toggle_off_pressed_border),
                    ("toggle off disabled", colors.toggle_off_disabled_border),
                    ("toggle on", colors.toggle_on_border),
                    ("toggle on hover", colors.toggle_on_hover_border),
                    ("toggle on pressed", colors.toggle_on_pressed_border),
                    ("toggle on disabled", colors.toggle_on_disabled_border),
                ] {
                    assert_host_contrast(name, border, hosts, 3.0);
                }
                let primary_floor = if capabilities.increase_contrast {
                    7.0
                } else {
                    4.5
                };
                let disabled_floor = if capabilities.increase_contrast {
                    4.5
                } else {
                    3.0
                };
                for (name, label, minimum) in [
                    ("toggle off label", colors.toggle_off_label, primary_floor),
                    (
                        "toggle off hover label",
                        colors.toggle_off_hover_label,
                        primary_floor,
                    ),
                    (
                        "toggle off pressed label",
                        colors.toggle_off_pressed_label,
                        primary_floor,
                    ),
                    (
                        "toggle off disabled label",
                        colors.toggle_off_disabled_label,
                        disabled_floor,
                    ),
                    ("toggle on label", colors.toggle_on_label, primary_floor),
                    (
                        "toggle on hover label",
                        colors.toggle_on_hover_label,
                        primary_floor,
                    ),
                    (
                        "toggle on pressed label",
                        colors.toggle_on_pressed_label,
                        primary_floor,
                    ),
                    (
                        "toggle on disabled label",
                        colors.toggle_on_disabled_label,
                        disabled_floor,
                    ),
                ] {
                    assert_host_contrast(name, label, hosts, minimum);
                }
                let fields = &prepared.floating_field_colors;
                for (name, border) in [
                    ("input", fields.input_border),
                    ("input focused", fields.input_focused_border),
                    ("input invalid", fields.input_invalid_border),
                    ("input disabled", fields.input_disabled_border),
                ] {
                    assert_host_contrast(name, border, hosts, 3.0);
                }
                if capabilities.increase_contrast {
                    assert_host_contrast("focus ring", colors.focus_ring, hosts, 4.5);
                    assert_host_contrast("field focus ring", fields.focus_ring, hosts, 4.5);
                }

                let segmented = &prepared.floating_segmented_colors;
                let track_hosts = hosts.map(|host| segmented.element_background.source_over(host));
                for (name, border) in [
                    ("selection", segmented.selection_border),
                    ("selection hover", segmented.selection_hover_border),
                    ("selection pressed", segmented.selection_pressed_border),
                    ("selection disabled", segmented.selection_disabled_border),
                ] {
                    assert_host_contrast(name, border, track_hosts, 3.0);
                }
                if capabilities.increase_contrast {
                    assert_host_contrast("segmented focus ring", segmented.focus_ring, hosts, 4.5);
                }
            }
        }
    }
}

/// Resolves a black or white GPUI-content endpoint through the tone box and elevation wash.
///
/// The renderer may reduce that content's alpha afterward so the semantic native Light or Dark
/// material contributes the final backdrop. Arbitrary native pixels are outside this helper's
/// contrast contract.
fn shell_endpoint_background(shell: FloatingShell, underlay: Color) -> Color {
    assert!(
        underlay == Color::rgb(0x000000) || underlay == Color::rgb(0xffffff),
        "this helper models only the tone box endpoints"
    );
    let tone = Color::rgba(u32::from(shell.backdrop_tone()));
    let wash = Color::rgba(u32::from(shell.material()));
    wash.source_over(tone.source_over(underlay))
}

fn resolve_case(
    appearance: Appearance,
    density: ChromeDensity,
    transparency: f32,
    blur: bool,
    supported: bool,
) -> (ResolvedAppearance, ChromeAppearance) {
    resolve_case_with_transparency_accessibility(
        appearance,
        density,
        transparency,
        blur,
        supported,
        true,
    )
}

fn resolve_case_with_transparency_accessibility(
    appearance: Appearance,
    density: ChromeDensity,
    transparency: f32,
    blur: bool,
    supported: bool,
    accessibility_allows_transparency: bool,
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
                crate::appearance::CompositionCapabilities::new(
                    supported,
                    accessibility_allows_transparency,
                ),
            ),
            &AvailableFonts::default(),
        )
        .expect("valid built-in surface case should resolve");
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    (resolved, prepared)
}

#[test]
fn non_floating_material_controls_meet_content_floors_on_their_known_hosts() {
    let (_, prepared) = resolve_case(Appearance::Light, ChromeDensity::Compact, 1.0, true, true);

    for (host_name, host, paint, segmented) in [
        (
            "Window",
            prepared.control_host_background(spaceterm_ui::ControlHost::Window),
            &prepared.control_colors,
            &prepared.segmented_control_colors,
        ),
        (
            "TitleBar",
            prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar),
            &prepared.title_bar_controls.colors,
            &prepared.title_bar_controls.segmented,
        ),
        (
            "Panel",
            prepared.control_host_background(spaceterm_ui::ControlHost::Panel),
            &prepared.panel_controls.colors,
            &prepared.panel_controls.segmented,
        ),
        (
            "Card",
            prepared.control_host_background(spaceterm_ui::ControlHost::Card),
            &prepared.card_controls.colors,
            &prepared.card_controls.segmented,
        ),
    ] {
        for (state_name, fill, foreground, icon, minimum) in [
            (
                "element",
                paint.element_background,
                paint.element_foreground,
                paint.element_icon,
                4.5,
            ),
            (
                "element hover",
                paint.element_hover,
                paint.element_hover_foreground,
                paint.element_hover_icon,
                4.5,
            ),
            (
                "element pressed",
                paint.element_active,
                paint.element_active_foreground,
                paint.element_active_icon,
                4.5,
            ),
            (
                "element disabled",
                paint.element_disabled,
                paint.element_disabled_foreground,
                paint.element_disabled_icon,
                3.0,
            ),
            (
                "ghost",
                paint.ghost_element_background,
                paint.ghost_element_foreground,
                paint.ghost_element_icon,
                4.5,
            ),
            (
                "ghost hover",
                paint.ghost_element_hover,
                paint.ghost_element_hover_foreground,
                paint.ghost_element_hover_icon,
                4.5,
            ),
            (
                "ghost pressed",
                paint.ghost_element_active,
                paint.ghost_element_active_foreground,
                paint.ghost_element_active_icon,
                4.5,
            ),
            (
                "ghost disabled",
                paint.ghost_element_disabled,
                paint.ghost_element_disabled_foreground,
                paint.ghost_element_disabled_icon,
                3.0,
            ),
            (
                "primary",
                paint.primary_background,
                paint.primary_foreground,
                paint.primary_icon,
                4.5,
            ),
            (
                "primary hover",
                paint.primary_hover_background,
                paint.primary_hover_foreground,
                paint.primary_hover_icon,
                4.5,
            ),
            (
                "primary pressed",
                paint.primary_pressed_background,
                paint.primary_pressed_foreground,
                paint.primary_pressed_icon,
                4.5,
            ),
            (
                "primary disabled",
                paint.primary_disabled_background,
                paint.primary_disabled_foreground,
                paint.primary_disabled_icon,
                3.0,
            ),
            (
                "destructive",
                paint.destructive_background,
                paint.destructive_foreground,
                paint.destructive_icon,
                4.5,
            ),
            (
                "destructive hover",
                paint.destructive_hover_background,
                paint.destructive_hover_foreground,
                paint.destructive_hover_icon,
                4.5,
            ),
            (
                "destructive pressed",
                paint.destructive_pressed_background,
                paint.destructive_pressed_foreground,
                paint.destructive_pressed_icon,
                4.5,
            ),
            (
                "destructive disabled",
                paint.destructive_disabled_background,
                paint.destructive_disabled_foreground,
                paint.destructive_disabled_icon,
                3.0,
            ),
            (
                "selection",
                paint.selection_background,
                paint.selection_foreground,
                paint.selection_icon,
                4.5,
            ),
            (
                "selection hover",
                paint.selection_hover_background,
                paint.selection_hover_foreground,
                paint.selection_hover_icon,
                4.5,
            ),
            (
                "selection pressed",
                paint.selection_pressed_background,
                paint.selection_pressed_foreground,
                paint.selection_pressed_icon,
                4.5,
            ),
            (
                "selection disabled",
                paint.selection_disabled_background,
                paint.selection_disabled_foreground,
                paint.selection_disabled_icon,
                3.0,
            ),
        ] {
            let background = fill.source_over(host);
            for (content_name, content) in [("foreground", foreground), ("icon", icon)] {
                let rendered = content.source_over(background);
                let contrast = rendered.contrast_ratio(background);
                assert!(
                    contrast >= minimum,
                    "{host_name}/{state_name}/{content_name} must meet {minimum}:1 on its final known host: contrast={contrast:.2}, host={host:?}, fill={fill:?}, background={background:?}, content={content:?}",
                );
            }
        }

        for (state_name, fill, mark, label, minimum) in [
            (
                "toggle off",
                paint.toggle_off_background,
                paint.toggle_off_mark,
                paint.toggle_off_label,
                4.5,
            ),
            (
                "toggle off hover",
                paint.toggle_off_hover_background,
                paint.toggle_off_hover_mark,
                paint.toggle_off_hover_label,
                4.5,
            ),
            (
                "toggle off pressed",
                paint.toggle_off_pressed_background,
                paint.toggle_off_pressed_mark,
                paint.toggle_off_pressed_label,
                4.5,
            ),
            (
                "toggle off disabled",
                paint.toggle_off_disabled_background,
                paint.toggle_off_disabled_mark,
                paint.toggle_off_disabled_label,
                3.0,
            ),
            (
                "toggle on",
                paint.toggle_on_background,
                paint.toggle_on_mark,
                paint.toggle_on_label,
                4.5,
            ),
            (
                "toggle on hover",
                paint.toggle_on_hover_background,
                paint.toggle_on_hover_mark,
                paint.toggle_on_hover_label,
                4.5,
            ),
            (
                "toggle on pressed",
                paint.toggle_on_pressed_background,
                paint.toggle_on_pressed_mark,
                paint.toggle_on_pressed_label,
                4.5,
            ),
            (
                "toggle on disabled",
                paint.toggle_on_disabled_background,
                paint.toggle_on_disabled_mark,
                paint.toggle_on_disabled_label,
                3.0,
            ),
        ] {
            let fill = fill.source_over(host);
            assert!(
                mark.source_over(fill).contrast_ratio(fill) >= minimum,
                "{host_name}/{state_name} mark must meet {minimum}:1 on {fill:?}"
            );
            assert!(
                label.source_over(host).contrast_ratio(host) >= minimum,
                "{host_name}/{state_name} label must meet {minimum}:1 on {host:?}"
            );
        }

        let input = paint.input_background.source_over(host);
        for (name, content, minimum) in [
            ("input text", paint.input_text, 4.5),
            ("input placeholder", paint.input_placeholder, 4.5),
            ("input caret", paint.input_caret, 3.0),
        ] {
            assert!(
                content.source_over(input).contrast_ratio(input) >= minimum,
                "{host_name}/{name} must meet {minimum}:1 on {input:?}"
            );
        }
        let disabled_input = paint.input_disabled_background.source_over(host);
        assert!(
            paint
                .input_disabled_text
                .source_over(disabled_input)
                .contrast_ratio(disabled_input)
                >= 3.0,
            "{host_name}/disabled input text must meet 3:1 on {disabled_input:?}"
        );
        let progress_track = paint.progress_track.source_over(host);
        assert!(
            paint
                .progress_indicator
                .source_over(progress_track)
                .contrast_ratio(progress_track)
                >= 4.5,
            "{host_name}/progress indicator must meet 4.5:1 on {progress_track:?}"
        );

        let track = segmented.element_background.source_over(host);
        for (state_name, fill, foreground, minimum) in [
            ("segment", Color::rgba(0), segmented.text_secondary, 4.5),
            (
                "segment hover",
                segmented.ghost_element_hover,
                segmented.ghost_element_hover_foreground,
                4.5,
            ),
            (
                "segment pressed",
                segmented.ghost_element_active,
                segmented.ghost_element_active_foreground,
                4.5,
            ),
            (
                "segment disabled",
                Color::rgba(0),
                segmented.text_disabled,
                3.0,
            ),
            (
                "selected segment",
                segmented.selection_background,
                segmented.selection_foreground,
                4.5,
            ),
            (
                "selected segment hover",
                segmented.selection_hover_background,
                segmented.selection_hover_foreground,
                4.5,
            ),
            (
                "selected segment pressed",
                segmented.selection_pressed_background,
                segmented.selection_pressed_foreground,
                4.5,
            ),
            (
                "selected segment disabled",
                segmented.selection_disabled_background,
                segmented.selection_disabled_foreground,
                3.0,
            ),
        ] {
            let background = fill.source_over(track);
            assert!(
                foreground
                    .source_over(background)
                    .contrast_ratio(background)
                    >= minimum,
                "{host_name}/{state_name} must meet {minimum}:1 on its final track composite {background:?}"
            );
        }
    }
}

#[test]
fn non_floating_control_hosts_match_the_painted_surface_stack() {
    let (_, prepared) = resolve_case(Appearance::Light, ChromeDensity::Compact, 1.0, true, true);
    let sheet = prepared
        .surface(
            crate::appearance::SurfaceRole::Sheet,
            prepared.colors.background,
        )
        .source_over(prepared.colors.background);
    let window = prepared
        .surface(
            crate::appearance::SurfaceRole::Base,
            prepared.colors.background,
        )
        .source_over(sheet);

    assert_eq!(
        prepared.control_host_background(spaceterm_ui::ControlHost::Window),
        window
    );
    assert_eq!(
        prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar),
        prepared
            .surface(
                crate::appearance::SurfaceRole::Base,
                prepared.colors.title_bar_background,
            )
            .source_over(sheet)
    );
    assert_eq!(
        prepared.control_host_background(spaceterm_ui::ControlHost::Panel),
        prepared
            .surface(
                crate::appearance::SurfaceRole::Base,
                prepared.colors.panel_background,
            )
            .source_over(sheet)
    );
    assert_eq!(
        prepared.control_host_background(spaceterm_ui::ControlHost::Card),
        prepared
            .surface(
                crate::appearance::SurfaceRole::Surface,
                prepared.colors.elevated_surface_background,
            )
            .source_over(window)
    );
}

#[test]
fn native_window_support_does_not_disable_floating_translucency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, supported) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let (_, unsupported) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, false);

        for role in FLOATING_ROLES {
            let supported_shell = supported.floating_surfaces().shell(role);
            let unsupported_shell = unsupported.floating_surfaces().shell(role);
            let expected = supported_shell.material();
            assert!(
                expected.a < 1.0,
                "{appearance:?} {role:?} fixture must exercise translucency"
            );
            assert_eq!(unsupported_shell.material(), expected);
            assert_eq!(
                unsupported_shell.backdrop_tone(),
                supported_shell.backdrop_tone()
            );
            assert!(
                supported_shell.backdrop_alpha_limit() < 1.0,
                "{appearance:?} {role:?} must reveal the effective native backing"
            );
            assert_eq!(
                unsupported_shell.backdrop_alpha_limit(),
                1.0,
                "{appearance:?} {role:?} must retain content when no native backing is available"
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
                            assert_eq!(shell.backdrop_tone(), plain_shell.backdrop_tone());
                            assert_eq!(
                                shell.backdrop_alpha_limit(),
                                plain_shell.backdrop_alpha_limit(),
                                "Blur must not control native-backing transmission"
                            );
                            assert_eq!(
                                shell.backdrop_alpha_limit() < 1.0,
                                supported && transparency > 0.0,
                                "only effective native glass can admit the window backing"
                            );
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
                                assert_eq!(
                                    prepared.floating_surfaces().shell(role).backdrop_tone().a,
                                    0.0,
                                    "{role:?} must not filter an opaque fallback"
                                );
                                assert_eq!(
                                    prepared
                                        .floating_surfaces()
                                        .shell(role)
                                        .backdrop_alpha_limit(),
                                    1.0
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
fn maximum_floating_transparency_does_not_rebuild_an_opaque_slab_when_nested() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        for role in FLOATING_ROLES {
            let material = Color::rgba(u32::from(
                prepared.floating_surfaces().shell(role).material(),
            ));
            let nested = material.source_over(material.source_over(Color::rgba(0)));
            assert!(
                nested.a <= 40,
                "{appearance:?} {role:?} nested elevation washes must preserve the native-backed host, got {nested:?}"
            );
        }
        assert_eq!(prepared.colors.text.a, 255);
        assert_eq!(prepared.colors.text_muted.a, 255);
        assert_eq!(prepared.floating_colors.input_placeholder.a, 255);
    }
}

#[test]
fn floating_decoration_edges_transmit_glass_without_weakening_opaque_or_accessible_presentation() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, translucent) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let (_, opaque) = resolve_case(appearance, ChromeDensity::Compact, 0.0, true, true);

        let mut preferences = AppearancePreferences {
            mode: match appearance {
                Appearance::Light => AppearanceMode::Light,
                Appearance::Dark => AppearanceMode::Dark,
            },
            ..AppearancePreferences::default()
        };
        preferences.chrome.density = ChromeDensity::Compact;
        preferences.background.transparency = 1.0;
        preferences.background.blur = true;
        let accessible = SchemeCatalog::default()
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::available(appearance)
                    .with_composition(CompositionCapabilities::new(true, false)),
                &AvailableFonts::default(),
            )
            .expect("valid accessibility surface case should resolve");
        let accessible = ChromeAppearance::prepare(&accessible.chrome);

        for role in FLOATING_ROLES {
            let translucent_shell = translucent.floating_surfaces().shell(role);
            assert!(
                translucent_shell.edge().a < 1.0,
                "{appearance:?} {role:?} edge must transmit the native-backed host"
            );
            assert!(
                translucent_shell.divider().a < 1.0,
                "{appearance:?} {role:?} divider must transmit the native-backed host"
            );

            let opaque_shell = opaque.floating_surfaces().shell(role);
            let edge_band = match appearance {
                Appearance::Light => 1.20..=1.30,
                Appearance::Dark => 1.25..=1.50,
            };
            for shell in [opaque_shell, translucent_shell] {
                for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                    let host = shell_endpoint_background(shell, underlay);
                    let edge = Color::rgba(u32::from(shell.edge()));
                    assert!(
                        edge_band.contains(&edge.source_over(host).contrast_ratio(host)),
                        "{appearance:?} {role:?} floating boundary uses its surface-edge policy"
                    );
                }
            }
            let host = shell_endpoint_background(opaque_shell, Color::rgb(0));
            let divider = Color::rgba(u32::from(opaque_shell.divider()));
            let expected_divider_band = match appearance {
                Appearance::Light => 1.12..=1.22,
                Appearance::Dark => 1.15..=1.35,
            };
            assert!(
                expected_divider_band.contains(&divider.source_over(host).contrast_ratio(host)),
                "{appearance:?} {role:?} divider is prepared independently of the outer edge"
            );

            let accessible_shell = accessible.floating_surfaces().shell(role);
            assert_eq!(
                accessible_shell.edge(),
                opaque_shell.edge(),
                "{appearance:?} {role:?} accessibility fallback must restore the opaque edge"
            );
            assert_eq!(
                accessible_shell.divider(),
                opaque_shell.divider(),
                "{appearance:?} {role:?} accessibility fallback must restore the opaque divider"
            );
        }
    }
}

#[test]
fn floating_control_states_transmit_their_host_until_the_opaque_override() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, translucent) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let paint = &translucent.floating_control_colors;
        for (state, color) in [
            ("button", paint.element_background),
            ("button hover", paint.element_hover),
            ("button pressed", paint.element_active),
            ("button disabled", paint.element_disabled),
            ("trigger", paint.ghost_element_background),
            ("toggle", paint.toggle_off_background),
            ("toggle hover", paint.toggle_off_hover_background),
            ("field", paint.input_background),
            ("disabled field", paint.input_disabled_background),
        ] {
            assert!(
                color.a < 255,
                "{appearance:?} {state} must leave the floating host visible: {color:?}"
            );
        }
        if translucent
            .floating_fallbacks
            .contains(&FloatingControlFamily::GhostElement)
        {
            assert_eq!(paint.ghost_element_hover.a, 255);
            assert_eq!(paint.ghost_element_active.a, 255);
        } else {
            assert!(paint.ghost_element_hover.a < 255);
            assert!(paint.ghost_element_active.a < 255);
        }
        let segmented_selection = translucent.floating_segmented_colors.selection_background;
        if appearance == Appearance::Light {
            assert!(
                segmented_selection.a < 255,
                "the built-in Light floating segment must not force its FAFAFA base opaque"
            );
        }
        if !translucent
            .floating_fallbacks
            .contains(&FloatingControlFamily::Segmented)
        {
            assert!(segmented_selection.a < 255);
        }
        assert_ne!(paint.element_background, paint.element_hover);
        assert_ne!(paint.element_hover, paint.element_active);

        let (_, opaque) = resolve_case(appearance, ChromeDensity::Compact, 0.0, true, true);
        for paint in [
            opaque.floating_control_colors.element_hover,
            opaque.floating_control_colors.ghost_element_hover,
            opaque.floating_control_colors.input_background,
        ] {
            assert_eq!(paint.a, 255);
        }
    }
}

#[test]
fn built_in_control_backgrounds_transmit_at_the_default_transparency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 0.35, true, true);
        for (host, colors) in [
            ("Window", &prepared.control_colors),
            ("TitleBar", &prepared.title_bar_controls.colors),
            ("Panel", &prepared.panel_controls.colors),
            ("Card", &prepared.card_controls.colors),
            ("Floating", &prepared.floating_control_colors),
        ] {
            for (state, fill) in [
                ("button", colors.element_background),
                ("primary", colors.primary_background),
                ("destructive", colors.destructive_background),
                ("field", colors.input_background),
                ("selection", colors.selection_background),
                ("toggle", colors.toggle_off_background),
            ] {
                assert!(
                    fill.a < 255,
                    "{appearance:?}/{host}/{state} forced opaque: {fill:?}"
                );
            }
        }
    }
}

#[test]
fn dark_floating_ordinary_controls_share_one_readable_ordered_alpha() {
    let (resolved, prepared) =
        resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    assert_eq!(
        (
            resolved.chrome.colors.element_background,
            resolved.chrome.colors.element_hover,
            resolved.chrome.colors.element_active,
        ),
        (
            Color::rgb(0x272727),
            Color::rgb(0x2f2f2f),
            Color::rgb(0x363636),
        ),
        "floating preparation must not rewrite authored built-in values",
    );

    let paint = &prepared.floating_control_colors;
    let fills = [
        paint.element_background,
        paint.element_hover,
        paint.element_active,
    ];
    assert_eq!(fills.map(|fill| fill.a), [fills[0].a; 3]);
    assert!(fills[0].a < 255);

    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
    for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
        let host = shell_endpoint_background(shell, underlay);
        let backgrounds = fills.map(|fill| fill.source_over(host));
        assert!(backgrounds[0].contrast_ratio(backgrounds[1]) >= 1.05);
        assert!(backgrounds[1].contrast_ratio(backgrounds[2]) >= 1.05);
        assert!(backgrounds[0].r < backgrounds[1].r);
        assert!(backgrounds[1].r < backgrounds[2].r);
        for (background, foreground, icon, border) in [
            (
                backgrounds[0],
                paint.element_foreground,
                paint.element_icon,
                paint.element_border,
            ),
            (
                backgrounds[1],
                paint.element_hover_foreground,
                paint.element_hover_icon,
                paint.element_hover_border,
            ),
            (
                backgrounds[2],
                paint.element_active_foreground,
                paint.element_active_icon,
                paint.element_active_border,
            ),
        ] {
            assert!(
                foreground
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5
            );
            assert!(icon.source_over(background).contrast_ratio(background) >= 4.5);
            if border.a != 0 {
                assert!(border.source_over(background).contrast_ratio(background) >= 3.0);
            }
        }
    }
}

#[test]
fn dark_floating_ghost_states_preserve_authored_order_without_an_opaque_fallback() {
    let (resolved, prepared) =
        resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    let paint = &prepared.floating_control_colors;
    assert_eq!(paint.ghost_element_background.a, 0);
    let rest = prepared.floating_colors.elevated_surface_background;
    let hover = paint.ghost_element_hover;
    let pressed = paint.ghost_element_active;
    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
    let endpoints = [Color::rgb(0), Color::rgb(0xffffff)]
        .map(|underlay| shell_endpoint_background(shell, underlay));
    assert!(
        !prepared
            .floating_fallbacks
            .contains(&FloatingControlFamily::GhostElement),
        "Dark Ghost unexpectedly fell back: semantic_rest={rest:?}, host_endpoints={endpoints:?}, rest={:?}, hover={hover:?}, pressed={pressed:?}, disabled={:?}, hover_content={:?}, pressed_content={:?}",
        paint.ghost_element_background,
        paint.ghost_element_disabled,
        paint.ghost_element_hover_foreground,
        paint.ghost_element_active_foreground,
    );

    assert!(rest.r < hover.r && hover.r < pressed.r);
    assert!(hover.a < 255 && pressed.a < 255);

    let authored = &resolved.chrome.colors;
    for (authored_state, rehosted_state) in [
        (authored.ghost_element_hover, hover),
        (authored.ghost_element_active, pressed),
    ] {
        let authored_step = authored_state.contrast_ratio(authored.background);
        let rehosted_step = rehosted_state.source_over(rest).contrast_ratio(rest);
        assert!(
            (authored_step - rehosted_step).abs() < 0.02,
            "authored step {authored_step:.3} must survive rehosting as {rehosted_step:.3}"
        );
    }
}

#[test]
fn built_in_floating_ghost_states_transmit_without_weakening_order_or_content() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.35, 0.7, 1.0] {
            let (resolved, prepared) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let colors = &prepared.floating_control_colors;
            assert!(
                !prepared
                    .floating_fallbacks
                    .contains(&FloatingControlFamily::GhostElement),
                "{appearance:?} Ghost must not require an opaque fallback at {transparency}"
            );
            assert!(
                colors.ghost_element_hover.a < 255 && colors.ghost_element_active.a < 255,
                "{appearance:?} Ghost states must transmit at {transparency}: hover={:?}, pressed={:?}",
                colors.ghost_element_hover,
                colors.ghost_element_active,
            );

            let reference = [
                resolved.chrome.colors.background,
                resolved.chrome.colors.ghost_element_hover,
                resolved.chrome.colors.ghost_element_active,
            ];
            let reference_order = [
                reference[0].r.cmp(&reference[1].r),
                reference[0].r.cmp(&reference[2].r),
                reference[1].r.cmp(&reference[2].r),
            ];
            let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
            for underlay in [Color::rgb(0), Color::rgb(0xffffff)] {
                let host = shell_endpoint_background(shell, underlay);
                let rendered = [
                    host,
                    colors.ghost_element_hover.source_over(host),
                    colors.ghost_element_active.source_over(host),
                ];
                assert_eq!(
                    [
                        rendered[0].r.cmp(&rendered[1].r),
                        rendered[0].r.cmp(&rendered[2].r),
                        rendered[1].r.cmp(&rendered[2].r),
                    ],
                    reference_order,
                    "{appearance:?} Ghost order changed over {underlay:?} at {transparency}"
                );
                for (background, foreground, icon) in [
                    (
                        rendered[1],
                        colors.ghost_element_hover_foreground,
                        colors.ghost_element_hover_icon,
                    ),
                    (
                        rendered[2],
                        colors.ghost_element_active_foreground,
                        colors.ghost_element_active_icon,
                    ),
                ] {
                    assert!(
                        foreground
                            .source_over(background)
                            .contrast_ratio(background)
                            >= 4.5
                    );
                    assert!(icon.source_over(background).contrast_ratio(background) >= 4.5);
                }
            }
        }
    }
}

#[test]
fn builtin_ghost_seeds_clear_the_authored_separation_floor() {
    for (appearance, root, hover, pressed) in [
        (
            Appearance::Dark,
            Color::rgb(0x151515),
            Color::rgb(0x1b1b1b),
            Color::rgb(0x212121),
        ),
        (
            Appearance::Light,
            Color::rgb(0xe5e5e5),
            Color::rgb(0xd2d2d2),
            Color::rgb(0xc4c4c4),
        ),
    ] {
        let (resolved, _) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let colors = &resolved.chrome.colors;
        assert_eq!(colors.background, root);
        assert_eq!(colors.ghost_element_hover, hover);
        assert_eq!(colors.ghost_element_active, pressed);
        assert!(root.contrast_ratio(hover) >= 1.05);
        assert!(hover.contrast_ratio(pressed) >= 1.05);
    }
}

#[test]
fn panel_and_card_controls_compile_against_their_immediate_hosts() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, translucent) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        for (name, host, expected_background) in [
            (
                "Panel",
                &translucent.panel_controls,
                translucent.colors.panel_background,
            ),
            (
                "Card",
                &translucent.card_controls,
                translucent.colors.elevated_surface_background,
            ),
        ] {
            assert_eq!(host.reference.background, expected_background);
            for (state, fill) in [
                ("button", host.colors.element_background),
                ("button hover", host.colors.element_hover),
                ("button pressed", host.colors.element_active),
                ("trigger hover", host.colors.ghost_element_hover),
                ("field", host.colors.input_background),
                ("segmented track", host.segmented.element_background),
                ("segmented option", host.segmented.selection_background),
            ] {
                assert!(
                    fill.a < 255,
                    "{appearance:?} {name} {state} must transmit its immediate host: {fill:?}"
                );
            }
            assert_ne!(host.colors.element_background, host.colors.element_hover);
            assert_ne!(host.colors.element_hover, host.colors.element_active);
        }

        let (_, opaque) = resolve_case(appearance, ChromeDensity::Compact, 0.0, true, true);
        for host in [&opaque.panel_controls, &opaque.card_controls] {
            assert_eq!(host.colors.background, host.reference.background);
            for (state, actual, expected) in [
                (
                    "button",
                    host.colors.element_background,
                    host.reference.element_background,
                ),
                (
                    "button hover",
                    host.colors.element_hover,
                    host.reference.element_hover,
                ),
                (
                    "button pressed",
                    host.colors.element_active,
                    host.reference.element_active,
                ),
                (
                    "ghost hover",
                    host.colors.ghost_element_hover,
                    host.reference.ghost_element_hover,
                ),
                (
                    "primary",
                    host.colors.primary_background,
                    host.reference.primary_background,
                ),
                (
                    "destructive",
                    host.colors.destructive_background,
                    host.reference.destructive_background,
                ),
                (
                    "selection",
                    host.colors.selection_background,
                    host.reference.selection_background,
                ),
                (
                    "toggle off",
                    host.colors.toggle_off_background,
                    host.reference.toggle_off_background,
                ),
                (
                    "toggle on",
                    host.colors.toggle_on_background,
                    host.reference.toggle_on_background,
                ),
                (
                    "field",
                    host.colors.input_background,
                    host.reference.input_background,
                ),
                (
                    "disabled field",
                    host.colors.input_disabled_background,
                    host.reference.input_disabled_background,
                ),
            ] {
                assert_eq!(
                    actual, expected,
                    "{appearance:?} opaque {state} must keep its semantic fill"
                );
            }
            assert_eq!(
                host.segmented.element_background,
                if appearance == Appearance::Light {
                    host.reference.segmented_track_background
                } else {
                    host.reference.element_background
                },
                "opaque segmented compilation keeps the actual track"
            );
            assert_eq!(
                host.segmented.ghost_element_background.a, 0,
                "an unpainted option remains an unpainted sentinel"
            );
        }
    }

    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    let input = Color::rgba(0x20406080);
    let disabled_input = Color::rgba(0x60402060);
    let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
    authored.input_background = input;
    authored.input_disabled_background = disabled_input;
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let root = resolved.chrome.colors.background.with_alpha(255);
    let black = Color::rgb(0);
    for host in [&prepared.panel_controls, &prepared.card_controls] {
        for (authored, actual) in [
            (input, host.reference.input_background),
            (disabled_input, host.reference.input_disabled_background),
        ] {
            let authored_step =
                authored.source_over(root).contrast_ratio(black) / root.contrast_ratio(black);
            let actual_step =
                actual.contrast_ratio(black) / host.reference.background.contrast_ratio(black);
            assert!(
                // Each semantic endpoint is quantized independently to eight-bit sRGB. The
                // resulting ratio can move by a little over one hundredth at this luminance.
                (authored_step - actual_step).abs() < 0.015,
                "authored root-relative field step must survive rehosting: {authored_step} versus {actual_step}"
            );
        }
    }
    assert_eq!(resolved.chrome.colors.input_background, input);
    assert_eq!(
        resolved.chrome.colors.input_disabled_background,
        disabled_input
    );
}

#[test]
fn segmented_options_preserve_their_authored_step_against_each_actual_track() {
    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 0.0, true, true);
    let colors = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
    colors.background = Color::rgb(0x181818);
    colors.panel_background = Color::rgb(0x202020);
    colors.elevated_surface_background = Color::rgb(0x282828);
    colors.element_background = Color::rgb(0x303030);
    colors.selection_background = Color::rgb(0x383838);
    colors.selection_hover_background = Color::rgb(0x404040);
    colors.selection_pressed_background = Color::rgb(0x484848);
    colors.selection_disabled_background = Color::rgb(0x505050);
    let authored = colors.clone();

    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    let black = Color::rgb(0);
    let authored_root = authored.background.with_alpha(255);
    for (host_name, segmented) in [
        ("Window", &prepared.segmented_control_colors),
        ("Panel", &prepared.panel_controls.segmented),
        ("Card", &prepared.card_controls.segmented),
        ("Floating", &prepared.floating_segmented_colors),
    ] {
        let track = segmented.element_background;
        assert!(
            track.is_opaque(),
            "opaque fixture must expose semantic targets"
        );
        for (state_name, authored_state, actual_state) in [
            (
                "resting",
                authored.selection_background,
                segmented.selection_background,
            ),
            (
                "hover",
                authored.selection_hover_background,
                segmented.selection_hover_background,
            ),
            (
                "pressed",
                authored.selection_pressed_background,
                segmented.selection_pressed_background,
            ),
        ] {
            let authored_step =
                authored_state.contrast_ratio(black) / authored_root.contrast_ratio(black);
            let actual_step = actual_state.contrast_ratio(black) / track.contrast_ratio(black);
            assert!(
                // The track and option endpoint are quantized independently to eight-bit sRGB.
                // Panel/hover differs by about 0.0127 while preserving the intended step.
                (authored_step - actual_step).abs() < 0.015,
                "{host_name}/{state_name} must preserve the authored option/root step against its actual track: {authored_step:.4} versus {actual_step:.4}"
            );
        }
        let enabled_selected = segmented.selection_background;
        let disabled_selected = segmented.selection_disabled_background;
        assert_eq!(
            enabled_selected.r >= track.r,
            disabled_selected.source_over(track).r >= track.r,
            "{host_name}/disabled must preserve the enabled selected chip direction"
        );
        assert!(
            disabled_selected.source_over(track).contrast_ratio(track) >= 1.12,
            "{host_name}/disabled must keep the selected chip step"
        );
    }
}

#[test]
fn segmented_options_use_a_translucent_authored_track_composited_over_its_root() {
    let (mut resolved, _) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 0.0, true, true);
    let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
    chrome.colors.background = Color::rgb(0x182838);
    chrome.colors.segmented_track_background = Color::rgba(0xb0603080);
    chrome.colors.selection_background = Color::rgba(0xf0c070c0);
    chrome
        .provenance
        .insert("segmented_track_background", ColorProvenance::Authored);
    let authored_track = chrome
        .colors
        .segmented_track_background
        .source_over(chrome.colors.background);
    let authored_selection = chrome
        .colors
        .selection_background
        .source_over(authored_track);

    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    assert_eq!(
        prepared.segmented_control_colors.element_background,
        authored_track,
    );
    assert_eq!(
        prepared.segmented_control_colors.selection_background,
        authored_selection,
    );
}

#[test]
fn floating_control_content_remains_readable_on_material_state_fills() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let colors = &prepared.floating_control_colors;
        let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
        let text_states = [
            (
                "button",
                colors.element_background,
                colors.element_foreground,
                4.5,
            ),
            (
                "button hover",
                colors.element_hover,
                colors.element_hover_foreground,
                4.5,
            ),
            (
                "button pressed",
                colors.element_active,
                colors.element_active_foreground,
                4.5,
            ),
            (
                "button disabled",
                colors.element_disabled,
                colors.element_disabled_foreground,
                3.0,
            ),
            (
                "ghost",
                colors.ghost_element_background,
                colors.ghost_element_foreground,
                4.5,
            ),
            (
                "ghost hover",
                colors.ghost_element_hover,
                colors.ghost_element_hover_foreground,
                4.5,
            ),
            (
                "ghost pressed",
                colors.ghost_element_active,
                colors.ghost_element_active_foreground,
                4.5,
            ),
            (
                "primary",
                colors.primary_background,
                colors.primary_foreground,
                4.5,
            ),
            (
                "primary hover",
                colors.primary_hover_background,
                colors.primary_hover_foreground,
                4.5,
            ),
            (
                "primary pressed",
                colors.primary_pressed_background,
                colors.primary_pressed_foreground,
                4.5,
            ),
            (
                "destructive",
                colors.destructive_background,
                colors.destructive_foreground,
                4.5,
            ),
            (
                "toggle off",
                colors.toggle_off_background,
                colors.toggle_off_mark,
                3.0,
            ),
            (
                "toggle off hover",
                colors.toggle_off_hover_background,
                colors.toggle_off_hover_mark,
                3.0,
            ),
            (
                "toggle on",
                colors.toggle_on_background,
                colors.toggle_on_mark,
                3.0,
            ),
            (
                "toggle on hover",
                colors.toggle_on_hover_background,
                colors.toggle_on_hover_mark,
                3.0,
            ),
        ];
        for (state, fill, foreground, minimum) in text_states {
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let host = shell_endpoint_background(shell, underlay);
                let background = fill.source_over(host);
                let contrast = foreground
                    .source_over(background)
                    .contrast_ratio(background);
                assert!(
                    contrast >= minimum,
                    "{appearance:?} {state} contrast={contrast:.2} on {background:?}"
                );
            }
        }
        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let host = shell_endpoint_background(shell, underlay);
            assert!(
                colors.text_accent.source_over(host).contrast_ratio(host) >= 4.5,
                "{appearance:?} bare-button accent must read over the host"
            );
            let progress_track = colors.toggle_off_background.source_over(host);
            assert!(
                colors
                    .text_accent
                    .source_over(progress_track)
                    .contrast_ratio(progress_track)
                    >= 4.5,
                "{appearance:?} progress accent must read over its track"
            );
            for (status, foreground) in [
                ("info", colors.info),
                ("success", colors.success),
                ("warning", colors.warning),
                ("error", colors.error),
            ] {
                assert!(
                    foreground.source_over(host).contrast_ratio(host) >= 3.0,
                    "{appearance:?} {status} indicator must read over the modal host"
                );
            }
        }
        let segmented = &prepared.floating_segmented_colors;
        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let host = shell_endpoint_background(shell, underlay);
            let track = segmented.element_background.source_over(host);
            for (state, fill, foreground, minimum) in [
                ("segment", Color::rgba(0), segmented.text_secondary, 4.5),
                (
                    "segment hover",
                    segmented.ghost_element_hover,
                    segmented.ghost_element_hover_foreground,
                    4.5,
                ),
                (
                    "segment pressed",
                    segmented.ghost_element_active,
                    segmented.ghost_element_active_foreground,
                    4.5,
                ),
                (
                    "selected segment",
                    segmented.selection_background,
                    segmented.selection_foreground,
                    4.5,
                ),
                (
                    "selected segment hover",
                    segmented.selection_hover_background,
                    segmented.selection_hover_foreground,
                    4.5,
                ),
                (
                    "selected segment pressed",
                    segmented.selection_pressed_background,
                    segmented.selection_pressed_foreground,
                    4.5,
                ),
                (
                    "selected segment disabled",
                    segmented.selection_disabled_background,
                    segmented.selection_disabled_foreground,
                    3.0,
                ),
            ] {
                let background = fill.source_over(track);
                let contrast = foreground
                    .source_over(background)
                    .contrast_ratio(background);
                assert!(
                    contrast >= minimum,
                    "{appearance:?} {state} contrast={contrast:.2} on {background:?}"
                );
            }
        }
    }
}

#[test]
fn unfocused_popup_selection_keeps_transmitting_the_material() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.35, 1.0] {
            let (_, prepared) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let mut popup = prepared.floating_colors.clone();
            popup.elevated_surface_background =
                prepared.floating_surface(prepared.floating_colors.elevated_surface_background);
            let unfocused = super::control_theme_catalog::popup_unfocused_rows(&prepared, &popup);
            let rows = super::control_theme_catalog::overlay_list_rows_with_policy(
                &unfocused.reference,
                &unfocused.paint,
                super::control_theme_catalog::OverlayRowPolicy::prepared(&prepared),
            );
            let selected = rows.resolve_for_collection(true, true, false, false);

            assert!(
                selected.background().a > 0.0 && selected.background().a < 1.0,
                "{appearance:?} transparency={transparency}: unfocused popup selection must remain translucent: {selected:?}; reference={:?}; paint={:?}; surface={:?}",
                unfocused.reference.row_selected_background,
                unfocused.paint.row_selected_background,
                unfocused.paint.elevated_surface_background,
            );
        }
    }
}

#[gpui::test]
fn installed_floating_catalog_uses_the_material_control_presentation(
    cx: &mut gpui::TestAppContext,
) {
    let (_, prepared) = resolve_case(Appearance::Dark, ChromeDensity::Compact, 1.0, true, true);
    let reference = &prepared.floating_colors;
    let colors = &prepared.floating_control_colors;
    let field = &prepared.floating_field_colors;
    let mut popup = reference.clone();
    popup.elevated_surface_background =
        prepared.floating_surface(prepared.floating_colors.elevated_surface_background);
    let unfocused_reference = prepared
        .unfocused_selection_colors(spaceterm_ui::ControlHost::Floating)
        .clone();
    let mut unfocused_popup = unfocused_reference.clone();
    unfocused_popup.elevated_surface_background = popup.elevated_surface_background;
    let with_elevation = |theme: spaceterm_ui::SurfaceControlThemes| {
        theme.toggle_segmented_elevation(
            spaceterm_ui::ControlShadow::none(),
            None,
            spaceterm_ui::ControlShadow::single(spaceterm_ui::ControlShadowLayer::new(
                gpui::rgba(prepared.colors.shadow.multiply_opacity(89).rgba_hex()).into(),
                gpui::px(0.0),
                gpui::px(1.0),
                gpui::px(2.0),
                gpui::px(-1.0),
            )),
            None,
        )
    };
    let expected = spaceterm_ui::SurfaceControlThemes::new(
        super::button_theme::prepared(
            colors,
            &prepared.typography,
            &prepared.icons,
            prepared.capabilities.show_borders,
        ),
        super::toggle_theme::prepared(colors, &prepared.typography),
        super::progress_theme::theme(colors, spaceterm_ui::ProgressMotion::Standard),
        super::segmented_control_theme::prepared(
            &prepared.floating_segmented_colors,
            &prepared.typography,
            prepared.capabilities.show_borders,
        ),
        super::search_field_theme::prepared(
            &prepared.floating_field_reference,
            field,
            &prepared.typography,
            &prepared.icons,
        ),
        super::text_input_theme::themed(field, reference),
    )
    .triggers(
        super::menu_theme::prepared_with_rows(
            reference,
            colors,
            &popup,
            Some((&unfocused_reference, &unfocused_popup)),
            &prepared.typography,
            &prepared.icons,
            super::control_theme_catalog::OverlayRowPolicy::prepared(&prepared),
        ),
        super::combo_box_theme::prepared_with_rows(
            reference,
            colors,
            &popup,
            Some((&unfocused_reference, &unfocused_popup)),
            &prepared.typography,
            &prepared.icons,
            super::control_theme_catalog::OverlayRowPolicy::prepared(&prepared),
        ),
    );
    let expected = with_elevation(expected);
    let expected_panel = super::control_theme_catalog::surface_control_themes(
        &prepared.panel_controls,
        reference,
        &popup,
        spaceterm_ui::ProgressMotion::Standard,
        &prepared,
    );
    let expected_title_bar = super::control_theme_catalog::surface_control_themes(
        &prepared.title_bar_controls,
        reference,
        &popup,
        spaceterm_ui::ProgressMotion::Standard,
        &prepared,
    );
    let expected_card = super::control_theme_catalog::surface_control_themes(
        &prepared.card_controls,
        reference,
        &popup,
        spaceterm_ui::ProgressMotion::Standard,
        &prepared,
    );
    let expected_panel = with_elevation(expected_panel);
    let expected_title_bar = with_elevation(expected_title_bar);
    let expected_card = with_elevation(expected_card);

    cx.update(|cx| {
        spaceterm_ui::init(
            cx,
            super::control_theme_catalog::catalog(
                &prepared,
                spaceterm_ui::ProgressMotion::Standard,
            ),
        )
        .expect("floating catalog should install");
        assert_eq!(
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .hosted_controls(spaceterm_ui::ControlHost::TitleBar),
            Some(&expected_title_bar),
        );
        assert_eq!(
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .hosted_controls(spaceterm_ui::ControlHost::Floating),
            Some(&expected),
        );
        assert_eq!(
            cx.global::<spaceterm_ui::ModalTheme>(),
            &super::modal_theme::theme(colors),
        );
        assert_eq!(
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .hosted_controls(spaceterm_ui::ControlHost::Panel),
            Some(&expected_panel),
        );
        assert_eq!(
            cx.global::<spaceterm_ui::ControlThemeCatalog>()
                .hosted_controls(spaceterm_ui::ControlHost::Card),
            Some(&expected_card),
        );
    });
}

#[test]
fn floating_wash_eases_from_opaque_to_thin_without_an_opacity_step() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let mut previous = 255_u8;
        for transparency in [0.0, 1.0 / 255.0, 0.03, 0.06, 0.12, 0.35, 1.0] {
            let (_, prepared) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let alpha = Color::rgba(u32::from(
                prepared
                    .floating_surfaces()
                    .shell(FloatingRole::Popover)
                    .material(),
            ))
            .a;
            assert!(
                alpha <= previous,
                "{appearance:?} wash coverage rose from {previous} to {alpha} at {transparency}"
            );
            if transparency == 1.0 / 255.0 {
                assert!(
                    255 - alpha <= 10,
                    "{appearance:?} first nonzero setting stepped from opaque to {alpha}"
                );
            }
            if transparency >= 0.12 {
                assert!(
                    alpha <= 20,
                    "{appearance:?} engaged glass must keep only a thin elevation wash, got {alpha}"
                );
            }
            previous = alpha;
        }
    }
}

#[test]
fn floating_text_stays_readable_over_extreme_content_at_maximum_transparency() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let host = &prepared.floating_colors;
        for role in FLOATING_ROLES {
            let shell = prepared.floating_surfaces().shell(role);
            let text = if role == FloatingRole::Readout {
                host.preview_foreground
            } else {
                host.text
            };
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = shell_endpoint_background(shell, underlay);
                let foreground = text.source_over(background);
                let contrast = foreground.contrast_ratio(background);
                assert!(
                    contrast >= 4.5,
                    "{appearance:?} {role:?} ordinary text must stay readable over {underlay:?}: \
                     contrast={contrast:.2}, background={background:?}"
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
            let shell = prepared.floating_surfaces().shell(role);
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = shell_endpoint_background(shell, underlay);
                let foreground = host.text_muted.source_over(background);
                let contrast = foreground.contrast_ratio(background);
                assert!(
                    contrast >= 4.5,
                    "{appearance:?} {role:?} supporting text must stay readable over {underlay:?}: \
                     contrast={contrast:.2}, background={background:?}"
                );
            }
        }
    }
}

#[test]
fn floating_disabled_content_stays_perceptible_without_matching_enabled_text() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let (_, prepared) = resolve_case(appearance, ChromeDensity::Compact, 1.0, true, true);
        let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
        for disabled in [
            prepared.floating_colors.text_disabled,
            prepared.floating_colors.icon_disabled,
        ] {
            assert_ne!(disabled, prepared.floating_colors.text);
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = shell_endpoint_background(shell, underlay);
                assert!(
                    disabled.contrast_ratio(background) >= 3.0,
                    "{appearance:?} disabled content {disabled:?} must remain perceptible over {background:?}"
                );
            }
        }
    }
}

#[test]
fn builtin_disabled_controls_never_report_an_unmet_absolute_floor() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for increase_contrast in [false, true] {
            for transparency in [0.0, 0.35, 1.0] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                std::sync::Arc::make_mut(&mut resolved.chrome)
                    .composition
                    .capabilities
                    .increase_contrast = increase_contrast;
                let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
                for prepared in [active, inactive] {
                    assert!(
                        !prepared
                            .disabled_diagnostics
                            .iter()
                            .any(|diagnostic| matches!(
                                diagnostic,
                                DisabledControlDiagnostic::ContrastFloor { .. }
                            )),
                        "{appearance:?}, active={}, IC={increase_contrast}, transparency={transparency}: {:?}",
                        prepared.active,
                        prepared.disabled_diagnostics,
                    );
                    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
                    let floating_hosts = [Color::rgb(0), Color::rgb(0xffffff)]
                        .map(|underlay| shell_endpoint_background(shell, underlay));
                    for (host_name, colors, hosts) in [
                        (
                            "Window",
                            &prepared.control_colors,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Window);
                                2],
                        ),
                        (
                            "TitleBar",
                            &prepared.title_bar_controls.colors,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::TitleBar);
                                2],
                        ),
                        (
                            "Panel",
                            &prepared.panel_controls.colors,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Panel); 2],
                        ),
                        (
                            "Card",
                            &prepared.card_controls.colors,
                            [prepared.control_host_background(spaceterm_ui::ControlHost::Card); 2],
                        ),
                        (
                            "Floating",
                            &prepared.floating_control_colors,
                            floating_hosts,
                        ),
                    ] {
                        let ordinary_cap = hosts
                            .into_iter()
                            .map(|host| {
                                colors
                                    .element_background
                                    .source_over(host)
                                    .contrast_ratio(host)
                            })
                            .fold(f64::INFINITY, f64::min);
                        for (family, fill, foreground, icon) in [
                            (
                                "Primary",
                                colors.primary_disabled_background,
                                colors.primary_disabled_foreground,
                                colors.primary_disabled_icon,
                            ),
                            (
                                "Destructive",
                                colors.destructive_disabled_background,
                                colors.destructive_disabled_foreground,
                                colors.destructive_disabled_icon,
                            ),
                        ] {
                            assert_eq!(
                                fill.r, fill.g,
                                "{appearance:?}/{host_name}/{family}: {fill:?}"
                            );
                            assert_eq!(
                                fill.g, fill.b,
                                "{appearance:?}/{host_name}/{family}: {fill:?}"
                            );
                            assert_eq!(foreground, icon);
                            for host in hosts {
                                let background = fill.source_over(host);
                                assert!(background.contrast_ratio(host) <= ordinary_cap + 1e-9);
                                assert!(
                                    foreground
                                        .source_over(background)
                                        .contrast_ratio(background)
                                        >= if increase_contrast { 4.5 } else { 3.0 },
                                    "{appearance:?}/{host_name}/{family}, active={}, IC={increase_contrast}, transparency={transparency}: fill={fill:?}, foreground={foreground:?}, host={host:?}",
                                    prepared.active,
                                );
                            }
                        }
                    }
                }
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
            let shell = prepared.floating_surfaces().shell(role);
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = shell_endpoint_background(shell, underlay);
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
            let shell = prepared.floating_surfaces().shell(role);
            let tone = Color::rgba(u32::from(shell.backdrop_tone()));
            let alpha = f32::from(tone.a) / 255.0;
            if matches!(role, FloatingRole::Tooltip | FloatingRole::Readout) {
                assert!(alpha >= 0.90);
            } else {
                assert!((0.65..=0.75).contains(&alpha));
            }
            assert_ne!(
                (tone.r, tone.g, tone.b),
                (midgray.r, midgray.g, midgray.b),
                "{appearance:?} {role:?} fixture must exercise the material fallback"
            );
            let foreground = if role == FloatingRole::Readout {
                host.preview_foreground
            } else {
                host.text
            };
            for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                let background = shell_endpoint_background(shell, underlay);
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
fn midgray_custom_material_remains_resolvable_while_glass_engages() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 1.0 / 255.0, 0.01, 0.05, 0.12] {
            let (mut resolved, _) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
            authored.elevated_surface_background = Color::rgb(0x808080);
            authored.preview_background = Color::rgb(0x808080);
            let prepared = ChromeAppearance::prepare(&resolved.chrome);
            for role in [FloatingRole::Popover, FloatingRole::Readout] {
                let shell = prepared.floating_surfaces().shell(role);
                let foreground = if role == FloatingRole::Readout {
                    prepared.floating_colors.preview_foreground
                } else {
                    prepared.floating_colors.text
                };
                for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                    let background = shell_endpoint_background(shell, underlay);
                    assert!(
                        foreground.contrast_ratio(background) >= 4.5,
                        "{appearance:?} {role:?} at {transparency} must resolve a readable neutral foreground"
                    );
                }
            }
        }
    }
}

#[test]
fn custom_floating_colors_remain_readable_across_material_engagement() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 1.0 / 255.0, 0.03, 0.06, 0.12, 0.35, 1.0] {
            for color in [
                0x404040, 0x707070, 0x808080, 0xa0a0a0, 0xff0000, 0x00ff00, 0x0000ff,
            ] {
                let (mut resolved, _) =
                    resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
                let authored = &mut std::sync::Arc::make_mut(&mut resolved.chrome).colors;
                authored.elevated_surface_background = Color::rgb(color);
                authored.preview_background = Color::rgb(color);
                let prepared = ChromeAppearance::prepare(&resolved.chrome);
                for role in [FloatingRole::Popover, FloatingRole::Readout] {
                    let shell = prepared.floating_surfaces().shell(role);
                    let foreground = if role == FloatingRole::Readout {
                        prepared.floating_colors.preview_foreground
                    } else {
                        prepared.floating_colors.text
                    };
                    for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
                        let background = shell_endpoint_background(shell, underlay);
                        assert!(
                            foreground.contrast_ratio(background) >= 4.5,
                            "{appearance:?} {role:?} color={color:#08x} transparency={transparency} foreground={foreground:?} background={background:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn floating_row_content_is_readable_on_idle_hovered_and_selected_backgrounds() {
    use super::control_theme_catalog::OverlayRow;

    for appearance in [Appearance::Light, Appearance::Dark] {
        for transparency in [0.0, 0.15, 0.35, 0.7, 1.0] {
            let (_, prepared) =
                resolve_case(appearance, ChromeDensity::Compact, transparency, true, true);
            let reference = prepared.floating_colors.clone();
            let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
            let material =
                prepared.floating_surface(prepared.floating_colors.elevated_surface_background);
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
                    let background = row
                        .fill
                        .source_over(shell_endpoint_background(shell, underlay));
                    for content in row.content {
                        assert!(
                            content.contrast_ratio(background) >= 4.5,
                            "{appearance:?} at {transparency}: row content {content:?} must read over {background:?}"
                        );
                    }
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
    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);
    let material = prepared.floating_surface(prepared.floating_colors.elevated_surface_background);
    assert_ne!(reference.elevated_surface_background, material);
    let idle = OverlayRow::resolve(
        (
            reference.elevated_surface_background,
            reference.elevated_surface_background,
        ),
        (material, material),
        [
            reference.row_foreground,
            reference.row_secondary,
            reference.row_icon,
            reference.row_match,
        ],
        reference.row_border,
    );
    assert_eq!(idle.fill.a, 0, "the idle row inherits its shell material");
    for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
        let background = idle
            .fill
            .source_over(shell_endpoint_background(shell, underlay));
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
    let field_reference = &prepared.floating_field_reference;
    let standard = &prepared.floating_field_colors;
    let shell = prepared.floating_surfaces().shell(FloatingRole::Popover);

    for foreground in [bare.input_text, bare.input_placeholder] {
        for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
            let background = shell_endpoint_background(shell, underlay);
            assert!(foreground.contrast_ratio(background) >= 4.5);
        }
    }
    for underlay in [Color::rgb(0x000000), Color::rgb(0xffffff)] {
        let shell_background = shell_endpoint_background(shell, underlay);
        let background = standard.input_background.source_over(shell_background);
        for foreground in [standard.input_text, standard.input_placeholder] {
            assert!(
                foreground
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5
            );
        }
        let disabled_background = standard
            .input_disabled_background
            .source_over(shell_background);
        assert!(
            standard
                .input_disabled_text
                .source_over(disabled_background)
                .contrast_ratio(disabled_background)
                >= 3.0
        );
        let selection_background = standard.input_selection_background.source_over(background);
        assert!(
            standard
                .input_selection_foreground
                .source_over(selection_background)
                .contrast_ratio(selection_background)
                >= 4.5
        );
    }
    assert!(
        standard.input_background.a < 255,
        "a Standard field must preserve its floating material"
    );
    assert_ne!(bare.input_text, standard.input_text);
    assert_ne!(
        super::text_input_theme::theme(bare),
        super::text_input_theme::themed(standard, bare),
        "floating Standard and Bare variants must keep distinct foregrounds"
    );

    let resting_disc = field_reference.input_placeholder.source_over(
        field_reference
            .input_background
            .source_over(field_reference.panel_background),
    );
    let clear_glyph = super::control_theme_catalog::readable_on(
        field_reference.input_background,
        resting_disc,
        4.5,
    );
    for disc in [
        standard.input_placeholder,
        standard.input_placeholder.mix(standard.input_text, 0.5),
        standard.input_text,
    ] {
        assert!(
            clear_glyph.contrast_ratio(disc) >= 4.5,
            "the clear glyph must remain readable on every enabled disc state"
        );
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
        let expected_material = prepared.floating_surface(expected_host);
        let repeated_material = prepared.floating_surface(doubled);
        assert_ne!(
            expected_material, repeated_material,
            "material adjustment must not hide repeated authored composition"
        );
        let reference = gpui::rgba(expected_material.rgba_hex());
        for role in FLOATING_ROLES {
            // Text-dense surfaces apply their own tone-alpha floor after this shared host solve.
            if matches!(role, FloatingRole::Tooltip | FloatingRole::Readout) {
                continue;
            }
            let shell = prepared.floating_surfaces().shell(role);
            let material = Color::rgba(u32::from(shell.material()))
                .source_over(Color::rgba(u32::from(shell.backdrop_tone())));
            let material = gpui::rgba(material.rgba_hex());
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
