use super::{
    ChromeAppearance, FloatingContrastFloors, floating_constraint, floating_host_constraint,
    floating_state, host_relative_fill, relative_luminance,
    resolve_floating_control_colors_detailed, resolve_floating_field_colors_detailed,
    resolve_floating_frame, resolve_floating_segmented_colors_detailed,
    resolve_floating_state_at_alpha, resolve_material_control_colors, settings,
};
use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeColors, Color, CompositionCapabilities, SystemAppearance, ThemeCatalog,
};

fn resolve_light(transparency: f32) -> crate::appearance::ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.window.transparency = transparency;
    ThemeCatalog::default()
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
        assert_eq!(selected, Color::rgb(0xfdfdfd));
    }
    let shell = appearance
        .floating_surfaces()
        .shell(spaceterm_ui::FloatingRole::Popover);
    let tone = Color::rgba(u32::from(shell.backdrop_tone()));
    let wash = Color::rgba(u32::from(shell.material()));
    assert_eq!(
        wash.source_over(tone.source_over(Color::BLACK)),
        Color::rgb(0xe5e5e5)
    );
}

/// Hover is read over the shell the window renders, so that is where it has to hold its step.
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
            let chip = crate::ui::selection_chip::ChipPaint {
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

#[test]
fn light_settings_uses_the_workspace_content_hierarchy() {
    use super::settings::{
        self,
        SettingsSurfaceRole::{Canvas, Card, Sidebar},
    };

    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_light(transparency);
        let (active, inactive) = ChromeAppearance::prepare_variants(&resolved.chrome);
        let content = active.pane_surface(resolved.terminal.colors.background);
        let raised = active.colors.elevated_surface_background;
        let (settings, _) = settings::prepare_variants(&resolved.chrome, active, inactive);
        let sidebar = settings.surface(Sidebar);
        let canvas = settings.surface(Canvas);
        let card = settings.surface(Card);

        assert_eq!(
            card.semantic, raised,
            "a Settings group must take the raised Chrome tone"
        );
        assert!(
            resolved.terminal.colors.background.r > card.semantic.r,
            "the reading surface stays the brighter of the two content surfaces"
        );
        assert!(
            canvas.paint.a >= content.a,
            "the Settings canvas must keep at least the Pane's ink at {transparency}: canvas={:?}, pane={content:?}",
            canvas.paint,
        );
        assert!(
            sidebar.background.r < canvas.background.r && canvas.background.r < card.background.r,
            "Light hierarchy at {transparency}: muted navigation {sidebar:?}, page {canvas:?}, raised group {card:?}"
        );
        for (name, lower, upper) in [
            ("navigation and page", sidebar.background, canvas.background),
            ("page and group", canvas.background, card.background),
        ] {
            assert!(
                upper.contrast_ratio(lower) >= 1.05,
                "Light {name} at {transparency} must separate by fill: {lower:?} then {upper:?}"
            );
        }
        assert_eq!(
            settings.card_edge().a,
            0,
            "Normal Light groups separate by fill instead of a decorative outline"
        );
    }
}

#[test]
fn light_settings_keeps_accessibility_group_boundaries() {
    for increase_contrast in [false, true] {
        let mut resolved = resolve_light(0.35);
        let chrome = std::sync::Arc::make_mut(&mut resolved.chrome);
        chrome.composition.capabilities.increase_contrast = increase_contrast;
        chrome.composition.capabilities.show_borders = !increase_contrast;
        let (active, inactive) = ChromeAppearance::prepare_variants(chrome);
        let (settings, _) = super::settings::prepare_variants(chrome, active, inactive);
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
        let (settings, _) = super::settings::prepare_variants(&resolved.chrome, active, inactive);
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

/// The Pane is the one surface that states a real boundary, so it keeps the stronger hairline.
#[test]
fn every_chip_lifts_without_drawing_a_border() {
    use crate::appearance::Appearance;

    let desktop = Color::rgb(0x2b3a55);
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
                let paint = crate::ui::selection_chip::ChipPaint {
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
                let strip = semantic_host.source_over(desktop);
                let painted = paint.fill.unwrap().source_over(strip);
                let chip_lift = lift(rim, painted);
                assert!(
                    chip_lift >= 0.03,
                    "{mode:?} at {transparency}: the {name} lift vanishes at {chip_lift:.3}"
                );
                let rimmed = rim.source_over(painted);
                let inward = (i32::from(painted.r) - i32::from(rimmed.r)).signum()
                    == (i32::from(painted.r) - i32::from(strip.r)).signum();
                assert_eq!(
                    inward,
                    resolved.chrome.appearance == Appearance::Light,
                    "{mode:?} at {transparency}: the {name} rim states its edge the wrong way: \
                     strip={strip:?} fill={painted:?} rim={rimmed:?}"
                );
                // The rim is a fraction of the chip's own step from its strip. Past that step it
                // reaches the strip's tone and reads as a line rather than as the chip's edge.
                let chip_step = painted.contrast_ratio(strip) - 1.0;
                assert!(
                    chip_lift <= chip_step * 0.6,
                    "{mode:?} at {transparency}: the {name} rim outruns the chip it belongs to \
                     at {chip_lift:.3} against the fill's own {chip_step:.3}"
                );
                assert!(
                    chip_lift <= pane_lift * 0.5,
                    "{mode:?} at {transparency}: the {name} rim reads as a border at \
                     {chip_lift:.3} against the Pane boundary's {pane_lift:.3}"
                );
            }
        }
    }
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
        let resolved = crate::appearance::ThemeCatalog::default()
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
            appearance.spacing(crate::ui::workspace_sidebar::SIDEBAR_ROW_SELECTION_INSET_Y),
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

#[gpui::test]
fn builtin_light_ordinary_control_edges_distinguish_hover_and_disabled(
    cx: &mut gpui::TestAppContext,
) {
    let mut preferences = crate::appearance::AppearancePreferences {
        mode: AppearanceMode::Light,
        ..Default::default()
    };
    preferences.window.transparency = 0.0;
    let resolved = ThemeCatalog::default()
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::available(Appearance::Light),
            &AvailableFonts::default(),
        )
        .unwrap();
    let prepared = ChromeAppearance::prepare(&resolved.chrome);
    cx.update(|cx| {
        crate::ui::initialize_controls(cx).unwrap();
        crate::ui::control_theme::replace_uniform_control_catalog(cx, &prepared).unwrap();
    });
    let paints = cx.update(|cx| {
        cx.global::<spaceterm_ui::ButtonTheme>()
            .paints(spaceterm_ui::ButtonVariant::Secondary)
    });
    let edge_strength = |paint: spaceterm_ui::ButtonPaint| {
        let fill =
            Color::rgba(u32::from(paint.background())).source_over(prepared.colors.background);
        Color::rgba(u32::from(paint.border()))
            .source_over(fill)
            .contrast_ratio(fill)
    };
    assert!(
        edge_strength(paints.hovered()) > edge_strength(paints.normal()) + 0.15,
        "Light ordinary control hover should visibly strengthen its edge, not merely change the host underneath the same border"
    );
    assert!(
        edge_strength(paints.disabled()) < edge_strength(paints.normal()),
        "Light disabled controls should have quieter edges than enabled controls"
    );
}

#[test]
fn builtin_light_inactive_navigation_and_segments_keep_raised_polarity() {
    let inactive_selection = Color::rgb(0xf4f4f4);
    for transparency in [0.0, 0.35, 1.0] {
        let resolved = resolve_light(transparency);
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
    let expected_paint =
        super::prominent_surface_with(materials, reference.segmented_track_background, elevated);
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
        let resolved = resolve_light(transparency);
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

        let shell = active
            .floating_surfaces()
            .shell(spaceterm_ui::FloatingRole::Popover);
        for underlay in [Color::BLACK, Color::WHITE] {
            let floating_host = Color::rgba(u32::from(shell.material()))
                .source_over(Color::rgba(u32::from(shell.backdrop_tone())).source_over(underlay));
            assert_active_segment_uses_elevated_surface(
                "Floating",
                transparency,
                &active,
                &active.floating_colors,
                &active.floating_segmented_colors,
                floating_host,
                true,
            );
        }

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

#[test]
fn material_frame_changes_fill_before_authored_content_polarity() {
    use crate::appearance::Color;

    let host = Color::WHITE;
    let opaque_seed = Color::rgb(0x0055aa);
    let translucent_fill = Color::rgba(0x0055aa80);
    let authored_content = Color::WHITE;
    let (fill, [content]) = resolve_floating_frame(
        opaque_seed,
        translucent_fill,
        host,
        [host; 2],
        [(authored_content, 4.5)],
    );
    let background = fill.source_over(host);

    assert_eq!(
        content, authored_content,
        "a representable opaque seed must preserve authored content polarity"
    );
    assert_ne!(
        fill, translucent_fill,
        "the prepared fill must strengthen before content changes"
    );
    assert!(content.contrast_ratio(background) >= 4.5);
}

#[test]
fn nonfloating_filled_polarity_flip_is_coherent_and_diagnosed() {
    use crate::appearance::{ChromeColors, Color};

    let host = Color::WHITE;
    let reference = ChromeColors {
        background: host,
        primary_background: Color::rgb(0xaaaaaa),
        primary_foreground: Color::WHITE,
        primary_icon: Color::rgb(0x111111),
        ..ChromeColors::default()
    };
    let resolved = resolve_material_control_colors(
        &reference,
        reference.clone(),
        host,
        host,
        FloatingContrastFloors::STANDARD,
        false,
    );
    let background = resolved.primary_background.source_over(host);

    for content in [resolved.primary_foreground, resolved.primary_icon] {
        assert!(relative_luminance(content) < relative_luminance(background));
        assert!(content.contrast_ratio(background) >= 4.5);
    }
    assert!(
        super::material_content_polarity_fallbacks(&reference, &resolved, host)
            .contains(&super::FloatingControlFamily::Element)
    );
}

#[test]
fn material_control_resolution_escapes_an_infeasible_increased_contrast_fill() {
    use crate::appearance::{ChromeColors, Color};
    let host = Color::WHITE;
    let reference = ChromeColors {
        background: host,
        primary_background: Color::rgb(0x767676),
        primary_foreground: Color::WHITE,
        primary_icon: Color::WHITE,
        ..ChromeColors::default()
    };
    let paint = ChromeColors {
        primary_background: Color::rgba(0x00000089),
        ..reference.clone()
    };

    let resolved = resolve_material_control_colors(
        &reference,
        paint,
        host,
        host,
        FloatingContrastFloors::INCREASED,
        true,
    );
    let background = resolved.primary_background.source_over(host);

    assert!(
        resolved
            .primary_foreground
            .source_over(background)
            .contrast_ratio(background)
            >= 7.0
    );
    assert!(
        resolved
            .primary_icon
            .source_over(background)
            .contrast_ratio(background)
            >= 7.0
    );
    assert_ne!(
        background,
        Color::rgb(0x767676),
        "an opaque midtone reference cannot carry 7:1 content"
    );
}

#[test]
fn inactive_material_resolution_hides_focus_and_preserves_invalid_border() {
    use crate::appearance::{ChromeColors, Color};
    let host = Color::WHITE;
    let reference = ChromeColors {
        background: host,
        focus_ring: Color::rgba(0),
        input_border: Color::rgba(0x10101080),
        input_invalid_border: Color::rgb(0xaa0000),
        ..ChromeColors::default()
    };
    let mut floors = FloatingContrastFloors::STANDARD;
    floors.interactive = false;
    let resolved =
        resolve_material_control_colors(&reference, reference.clone(), host, host, floors, false);

    assert_eq!(resolved.focus_ring.a, 0);
    assert_ne!(
        resolved.input_invalid_border.a, 0,
        "invalid state remains independently visible while inactive"
    );
}

#[test]
fn active_material_resolution_preserves_explicitly_absent_state_edges() {
    use crate::appearance::{ChromeColors, Color};
    let host = Color::WHITE;
    let absent = Color::rgba(0x12345600);
    let reference = ChromeColors {
        background: host,
        focus_ring: absent,
        input_invalid_border: absent,
        ..ChromeColors::default()
    };

    for floors in [
        FloatingContrastFloors::STANDARD,
        FloatingContrastFloors::INCREASED,
    ] {
        let resolved = resolve_material_control_colors(
            &reference,
            reference.clone(),
            host,
            host,
            floors,
            floors.focus.is_some(),
        );

        assert_eq!(resolved.focus_ring, absent);
        assert_eq!(resolved.input_invalid_border, absent);
    }
}

#[test]
fn floating_constraints_distinguish_inside_content_from_host_adjacent_paint() {
    use crate::appearance::Color;

    let white = Color::WHITE;
    let resolved = resolve_floating_state_at_alpha(
        floating_state(
            Color::BLACK,
            Color::BLACK,
            white,
            [
                floating_constraint(white, 4.5),
                floating_host_constraint(white, 4.5),
                None,
                None,
            ],
        ),
        255,
        [white; 2],
    )
    .expect("opposite inside and outside paints are jointly representable");

    assert_eq!(
        resolved.content[0], white,
        "inside content reads on the fill"
    );
    assert!(
        resolved.content[1].source_over(white).contrast_ratio(white) >= 4.5,
        "adjacent labels and perimeter strokes read on the host"
    );
}

#[test]
fn unpainted_floating_content_resolves_on_the_host_without_forcing_family_opacity() {
    use crate::appearance::Color;

    let host = Color::WHITE;
    let proposed = Color::rgb(0x999999);
    let resolved = resolve_floating_state_at_alpha(
        floating_state(
            Color::rgba(0),
            Color::rgba(0),
            host,
            [floating_constraint(proposed, 4.5), None, None, None],
        ),
        0,
        [host; 2],
    )
    .expect("unpainted content can move on its actual host without changing the fill");

    assert_eq!(resolved.fill.a, 0);
    assert!(resolved.content[0].contrast_ratio(host) >= 4.5);
}

#[test]
fn filled_fallback_keeps_label_and_icon_on_one_readable_polarity() {
    use crate::appearance::Color;

    let fill = Color::rgb(0x333333);
    let states = [floating_state(
        fill,
        fill,
        fill,
        [
            floating_constraint(Color::WHITE, 4.5),
            floating_constraint(Color::rgb(0x222222), 4.5),
            None,
            None,
        ],
    )];
    let (resolved, used_fallback) =
        super::resolve_floating_family_for_presentation(Ok(states), [fill; 2], &[])
            .expect("the opaque fill has a coherent readable endpoint");

    assert!(used_fallback);
    for content in [resolved[0].content[0], resolved[0].content[1]] {
        assert!(relative_luminance(content) > relative_luminance(fill));
        assert!(content.contrast_ratio(fill) >= 4.5);
    }
}

#[test]
fn fixed_polarity_readability_moves_an_already_readable_opposite_ink() {
    use crate::appearance::Color;

    let fill = Color::rgb(0x757575);
    let opposite = Color::BLACK;
    assert!(opposite.contrast_ratio(fill) >= 4.5);

    let resolved = super::readable_toward_endpoint(opposite, Color::WHITE, [fill], 4.5);

    assert!(relative_luminance(resolved) > relative_luminance(fill));
    assert!(resolved.contrast_ratio(fill) >= 4.5);
}

#[test]
fn panel_and_card_controls_preserve_the_authored_root_relative_step() {
    use crate::appearance::{ChromeColors, Color, CompositionCapabilities, SurfaceMaterials};
    let authored = ChromeColors::default();
    for control_host in [
        spaceterm_ui::ControlHost::Panel,
        spaceterm_ui::ControlHost::Card,
    ] {
        for host in [
            authored.background,
            Color::rgb(0x202020),
            Color::rgb(0x303030),
        ] {
            let prepared = super::prepare_state_control_host(
                &authored,
                (host, host),
                control_host,
                SurfaceMaterials::OPAQUE,
                super::ChromeStatePolicy {
                    active: true,
                    capabilities: CompositionCapabilities::default(),
                },
                super::FloatingContrastFloors::STANDARD,
                false,
            );
            for (fill, actual) in [
                (
                    authored.element_background,
                    prepared.reference.element_background,
                ),
                (authored.element_hover, prepared.reference.element_hover),
                (authored.element_active, prepared.reference.element_active),
            ] {
                assert_eq!(
                    relative_luminance(actual).partial_cmp(&relative_luminance(host)),
                    relative_luminance(fill).partial_cmp(&relative_luminance(authored.background)),
                );
                let authored_ratio = (relative_luminance(fill) + 0.05)
                    / (relative_luminance(authored.background) + 0.05);
                let actual_ratio =
                    (relative_luminance(actual) + 0.05) / (relative_luminance(host) + 0.05);
                assert!(
                    (authored_ratio - actual_ratio).abs() < 0.012,
                    "{control_host:?} host={host:?}: authored={authored_ratio}, actual={actual_ratio}"
                );
            }
        }
    }
}

#[test]
fn host_relative_fill_preserves_identity_and_mixed_direction_relationships() {
    use crate::appearance::Color;

    let root = Color::rgb(0x151515);
    let host = Color::rgb(0x202020);
    let authored = [
        Color::rgb(0x202020),
        Color::rgb(0x1d1d1d),
        Color::rgb(0x242424),
    ];
    assert_eq!(
        authored.map(|fill| host_relative_fill(fill, root, root).unwrap()),
        authored,
    );

    let rehosted = authored.map(|fill| host_relative_fill(fill, root, host).unwrap());
    for (left, right) in [(0, 1), (0, 2), (1, 2)] {
        assert_eq!(
            relative_luminance(authored[left]).partial_cmp(&relative_luminance(authored[right])),
            relative_luminance(rehosted[left]).partial_cmp(&relative_luminance(rehosted[right])),
        );
        let authored_ratio = (relative_luminance(authored[left]) + 0.05)
            / (relative_luminance(authored[right]) + 0.05);
        let rehosted_ratio = (relative_luminance(rehosted[left]) + 0.05)
            / (relative_luminance(rehosted[right]) + 0.05);
        assert!(
            (authored_ratio - rehosted_ratio).abs() < 0.012,
            "pair {left}-{right}: authored={authored_ratio}, rehosted={rehosted_ratio}, colors={rehosted:?}",
        );
    }
}

#[test]
fn host_relative_fill_handles_black_and_reports_out_of_gamut_steps() {
    use crate::appearance::Color;

    assert_eq!(
        host_relative_fill(Color::BLACK, Color::BLACK, Color::rgb(0x202020),),
        Some(Color::rgb(0x202020)),
    );
    assert_eq!(
        host_relative_fill(Color::WHITE, Color::BLACK, Color::WHITE,),
        None,
    );
}

#[test]
fn floating_fields_report_input_fallback_and_keep_endpoint_text_readable() {
    use crate::appearance::{ChromeColors, Color};

    let reference = ChromeColors {
        elevated_surface_background: Color::rgb(0x202020),
        input_background: Color::rgb(0x767676),
        input_text: Color::WHITE,
        input_placeholder: Color::WHITE,
        ..ChromeColors::default()
    };
    let paint = ChromeColors {
        input_background: Color::rgba(0x737373e7),
        ..reference.clone()
    };
    let resolution = resolve_floating_field_colors_detailed(
        &reference,
        FloatingContrastFloors::STANDARD,
        reference.clone(),
        paint,
        Color::rgba(0),
        Color::rgba(0),
    );

    assert!(
        resolution
            .fallback_families
            .contains(&super::FloatingControlFamily::Input)
    );
    let resolved = resolution.colors;
    for underlay in [Color::BLACK, Color::WHITE] {
        let background = resolved.input_background.source_over(underlay);
        assert!(
            resolved
                .input_text
                .source_over(background)
                .contrast_ratio(background)
                >= 4.5,
            "field text must read over {background:?}",
        );
    }
}

#[test]
fn floating_segment_labels_report_fallback_and_keep_endpoint_text_readable() {
    use crate::appearance::{ChromeColors, Color};

    let reference = ChromeColors {
        elevated_surface_background: Color::rgb(0x202020),
        element_background: Color::rgb(0x767676),
        text_secondary: Color::WHITE,
        ..ChromeColors::default()
    };
    let paint = ChromeColors {
        element_background: Color::rgba(0x737373e7),
        ..reference.clone()
    };
    let resolution = resolve_floating_segmented_colors_detailed(
        &reference,
        FloatingContrastFloors::STANDARD,
        &reference,
        paint,
        Color::rgba(0),
        Color::rgba(0),
        false,
    );

    assert!(
        resolution
            .fallback_families
            .contains(&super::FloatingControlFamily::Segmented)
    );
    let resolved = resolution.colors;
    for underlay in [Color::BLACK, Color::WHITE] {
        let background = resolved.element_background.source_over(underlay);
        assert!(
            resolved
                .text_secondary
                .source_over(background)
                .contrast_ratio(background)
                >= 4.5,
            "segment label must read over {background:?}",
        );
    }
}

#[test]
fn floating_toggle_fallback_keeps_accent_readable_on_host_and_track_endpoints() {
    use crate::appearance::{ChromeColors, Color};

    let reference = ChromeColors {
        elevated_surface_background: Color::rgb(0x606060),
        toggle_off_background: Color::rgb(0x767676),
        toggle_off_mark: Color::WHITE,
        text_accent: Color::WHITE,
        ..ChromeColors::default()
    };
    let paint = ChromeColors {
        toggle_off_background: Color::rgba(0xffffff1a),
        ..reference.clone()
    };
    let material = Color::rgba(0x666666ef);
    let resolution = resolve_floating_control_colors_detailed(
        &reference,
        FloatingContrastFloors::STANDARD,
        &reference,
        paint,
        material,
        Color::rgba(0),
    );

    assert!(
        resolution
            .fallback_families
            .contains(&super::FloatingControlFamily::Toggle)
    );
    let resolved = resolution.colors;
    for underlay in [Color::BLACK, Color::WHITE] {
        let host = material.source_over(underlay);
        let track = resolved.toggle_off_background.source_over(host);
        for background in [host, track] {
            assert!(
                resolved
                    .text_accent
                    .source_over(background)
                    .contrast_ratio(background)
                    >= 4.5,
                "progress accent must read over {background:?}",
            );
        }
    }
}

#[test]
fn floating_warning_border_keeps_its_role_and_resolves_against_the_host() {
    use crate::appearance::{ChromeColors, Color};

    let reference = ChromeColors {
        warning: Color::rgb(0xd02020),
        warning_border: Color::rgb(0xf0f0f0),
        ..ChromeColors::default()
    };
    let resolution = resolve_floating_control_colors_detailed(
        &reference,
        FloatingContrastFloors::STANDARD,
        &reference,
        reference.clone(),
        Color::rgba(0),
        Color::WHITE,
    );

    let resolved = resolution.colors;
    assert_ne!(resolved.warning_border, resolved.warning);
    assert!(resolved.warning_border.contrast_ratio(Color::WHITE) >= 3.0,);
}
