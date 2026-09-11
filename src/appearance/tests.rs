use std::collections::BTreeSet;

use super::*;

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
fn pane_caption_keeps_weight_400_when_chrome_weights_change() {
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
    assert_eq!(prepared.caption.weight, gpui::FontWeight::NORMAL);
    assert_eq!(prepared.caption.family, prepared.regular.family);
}

#[test]
fn dark_defaults_match_the_consumed_vague_pro_values() {
    let terminal = TerminalColors::default();
    assert_eq!(
        terminal.foreground,
        crate::theme::ACTIVE_THEME.terminal_foreground
    );
    assert_eq!(
        terminal.background,
        crate::theme::ACTIVE_THEME.terminal_background
    );
    assert_eq!(
        terminal.normal,
        crate::theme::ACTIVE_THEME.terminal_normal()
    );
    assert_eq!(
        terminal.bright,
        crate::theme::ACTIVE_THEME.terminal_bright()
    );
    assert_eq!(terminal.dim, crate::theme::ACTIVE_THEME.terminal_dim());
    assert_eq!(
        terminal.bright_foreground,
        crate::theme::ACTIVE_THEME.terminal_bright_foreground
    );
    assert_eq!(
        terminal.dim_foreground,
        crate::theme::ACTIVE_THEME.terminal_dim_foreground
    );

    let chrome = ChromeColors::default();
    let legacy = &*crate::theme::ACTIVE_THEME;
    let equivalent = [
        ("background", chrome.background, legacy.background),
        (
            "panel_background",
            chrome.panel_background,
            legacy.panel_background,
        ),
        (
            "elevated_surface_background",
            chrome.elevated_surface_background,
            legacy.elevated_surface_background,
        ),
        (
            "title_bar_background",
            chrome.title_bar_background,
            legacy.title_bar_background,
        ),
        (
            "title_bar_inactive_background",
            chrome.title_bar_inactive_background,
            legacy.title_bar_inactive_background,
        ),
        (
            "tab_active_background",
            chrome.tab_active_background,
            legacy.tab_active_background,
        ),
        (
            "tab_inactive_background",
            chrome.tab_inactive_background,
            legacy.tab_inactive_background,
        ),
        ("text", chrome.text, legacy.text),
        ("text_muted", chrome.text_muted, legacy.text_muted),
        (
            "text_placeholder",
            chrome.text_placeholder,
            legacy.text_placeholder,
        ),
        ("text_disabled", chrome.text_disabled, legacy.text_disabled),
        ("text_accent", chrome.text_accent, legacy.text_accent),
        (
            "link_text_hover",
            chrome.link_text_hover,
            legacy.link_text_hover,
        ),
        ("icon", chrome.icon, legacy.icon),
        ("icon_muted", chrome.icon_muted, legacy.icon_muted),
        ("icon_disabled", chrome.icon_disabled, legacy.icon_disabled),
        ("icon_accent", chrome.icon_accent, legacy.icon_accent),
        ("border", chrome.border, legacy.border),
        (
            "border_variant",
            chrome.border_variant,
            legacy.border_variant,
        ),
        (
            "border_focused",
            chrome.border_focused,
            legacy.border_focused,
        ),
        (
            "border_selected",
            chrome.border_selected,
            legacy.border_selected,
        ),
        (
            "border_disabled",
            chrome.border_disabled,
            legacy.border_disabled,
        ),
        (
            "border_transparent",
            chrome.border_transparent,
            legacy.border_transparent,
        ),
        (
            "element_background",
            chrome.element_background,
            legacy.element_background,
        ),
        ("element_hover", chrome.element_hover, legacy.element_hover),
        (
            "element_active",
            chrome.element_active,
            legacy.element_active,
        ),
        (
            "element_selected",
            chrome.element_selected,
            legacy.element_selected,
        ),
        (
            "element_disabled",
            chrome.element_disabled,
            legacy.element_disabled,
        ),
        (
            "ghost_element_background",
            chrome.ghost_element_background,
            legacy.ghost_element_background,
        ),
        (
            "ghost_element_hover",
            chrome.ghost_element_hover,
            legacy.ghost_element_hover,
        ),
        (
            "ghost_element_active",
            chrome.ghost_element_active,
            legacy.ghost_element_active,
        ),
        (
            "ghost_element_selected",
            chrome.ghost_element_selected,
            legacy.ghost_element_selected,
        ),
        (
            "ghost_element_disabled",
            chrome.ghost_element_disabled,
            legacy.ghost_element_disabled,
        ),
        ("info", chrome.info, legacy.info),
        (
            "info_background",
            chrome.info_background,
            legacy.info_background,
        ),
        ("success", chrome.success, legacy.success),
        ("warning", chrome.warning, legacy.warning),
        (
            "warning_background",
            chrome.warning_background,
            legacy.warning_background,
        ),
        (
            "warning_border",
            chrome.warning_border,
            legacy.warning_border,
        ),
        ("error", chrome.error, legacy.error),
        (
            "error_background",
            chrome.error_background,
            legacy.error_background,
        ),
        ("error_border", chrome.error_border, legacy.error_border),
        ("modal_scrim", chrome.modal_scrim, legacy.modal_scrim),
        (
            "scrollbar_track_border",
            chrome.scrollbar_track_border,
            legacy.scrollbar_track_border,
        ),
        (
            "scrollbar_thumb_background",
            chrome.scrollbar_thumb_background,
            legacy.scrollbar_thumb_background,
        ),
        (
            "scrollbar_thumb_border",
            chrome.scrollbar_thumb_border,
            legacy.scrollbar_thumb_border,
        ),
        (
            "scrollbar_thumb_hover_background",
            chrome.scrollbar_thumb_hover_background,
            legacy.scrollbar_thumb_hover_background,
        ),
    ];
    for (role, actual, expected) in equivalent {
        assert_eq!(actual, expected, "{role}");
    }
}

#[test]
fn four_dimensions_resolve_independently() {
    let defaults = AppearancePreferences::default();
    let baseline = resolve(&defaults);

    let mut chrome_colors = defaults.clone();
    chrome_colors.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.spaceterm.chrome.light"),
        appearance: Appearance::Light,
    };
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
    terminal_colors.terminal.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.spaceterm.terminal.light"),
        appearance: Appearance::Light,
    };
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
fn fixed_and_system_policies_cover_all_light_dark_pairs() {
    for chrome in [Appearance::Light, Appearance::Dark] {
        for terminal in [Appearance::Light, Appearance::Dark] {
            let mut preferences = AppearancePreferences::default();
            preferences.chrome.scheme = SchemeSelection::Fixed {
                id: match chrome {
                    Appearance::Light => SchemeId::builtin("builtin.spaceterm.chrome.light"),
                    Appearance::Dark => SchemeId::builtin("builtin.vague-pro.chrome.dark"),
                },
                appearance: chrome,
            };
            preferences.terminal.scheme = SchemeSelection::Fixed {
                id: match terminal {
                    Appearance::Light => SchemeId::builtin("builtin.spaceterm.terminal.light"),
                    Appearance::Dark => SchemeId::builtin("builtin.vague-pro.terminal.dark"),
                },
                appearance: terminal,
            };
            let resolved = resolve(&preferences);
            assert_eq!(resolved.chrome.appearance, chrome);
            assert_eq!(resolved.terminal.appearance, terminal);
        }
    }

    let mut preferences = AppearancePreferences::default();
    preferences.chrome.scheme = SchemeSelection::System {
        light: SchemeId::builtin("builtin.spaceterm.chrome.light"),
        dark: SchemeId::builtin("builtin.vague-pro.chrome.dark"),
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
    assert_eq!(dark.chrome.appearance, Appearance::Dark);
    assert_eq!(light.terminal, dark.terminal);
}

#[test]
fn unavailable_resources_preserve_requests_and_report_effective_fallbacks() {
    let missing = SchemeId::new("missing.chrome").unwrap();
    let mut preferences = AppearancePreferences::default();
    preferences.chrome.scheme = SchemeSelection::Fixed {
        id: missing.clone(),
        appearance: Appearance::Light,
    };
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
    preferences.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.vague-pro.terminal.dark"),
        appearance: Appearance::Dark,
    };
    assert!(matches!(
        SchemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::WrongSchemeKind)
    ));

    preferences.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.vague-pro.chrome.dark"),
        appearance: Appearance::Light,
    };
    assert!(matches!(
        SchemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::AppearanceMismatch)
    ));

    let mut document = AppearanceDocument::default();
    document.preferences.chrome.overrides.insert(
        SchemeId::builtin("builtin.vague-pro.terminal.dark"),
        ChromeColorOverrides::default(),
    );
    assert!(matches!(
        document.validate(),
        Err(AppearanceDocumentError::InvalidPreferences)
    ));

    let mut document = AppearanceDocument::default();
    document.preferences.terminal.overrides.insert(
        SchemeId::builtin("builtin.vague-pro.chrome.dark"),
        TerminalColorOverrides::default(),
    );
    assert!(matches!(
        document.validate(),
        Err(AppearanceDocumentError::InvalidPreferences)
    ));
}

fn reset_fixture() -> AppearanceDocument {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.spaceterm.chrome.light"),
        appearance: Appearance::Light,
    };
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

    document.preferences.terminal.scheme = SchemeSelection::Fixed {
        id: SchemeId::builtin("builtin.spaceterm.terminal.light"),
        appearance: Appearance::Light,
    };
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
            id: SchemeId::new("custom.reset-fixture").unwrap(),
            name: String::from("Reset Fixture"),
            appearance: Appearance::Dark,
            metadata: SchemeMetadata::default(),
            colors: ChromeColorOverrides::default(),
        })));
    document.validate().unwrap();
    document
}

#[test]
fn every_individual_preference_reset_changes_only_its_field() {
    type ExpectedEdit = fn(&mut AppearancePreferences);
    let defaults = AppearancePreferences::default();
    let cases: Vec<(ResetTarget, ExpectedEdit)> = vec![
        (ResetTarget::ChromeSchemeSelection, |value| {
            value.chrome.scheme = AppearancePreferences::default().chrome.scheme;
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
        (ResetTarget::TerminalSchemeSelection, |value| {
            value.terminal.scheme = AppearancePreferences::default().terminal.scheme;
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
    assert_eq!(cases.len(), 14);

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
            value.chrome.scheme = defaults.chrome.scheme;
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
            value.terminal.scheme = defaults.terminal.scheme;
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

    let mut sparse = AppearanceDocument::default();
    let chrome_id = SchemeId::builtin("builtin.vague-pro.chrome.dark");
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

    let terminal_id = SchemeId::builtin("builtin.vague-pro.terminal.dark");
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
    let document = AppearanceDocument::default();
    let encoded = export_settings(&document).unwrap();
    assert_eq!(parse_settings(encoded.as_bytes()).unwrap(), document);
    assert!(matches!(parse_settings(br#"{"schema_version":1,"schema_version":1,"revision":0,"preferences":{},"custom_schemes":[]}"#),
        Err(AppearanceDocumentError::DuplicateKey)));

    let unknown = encoded.replacen(
        "\"revision\": 0,",
        "\"revision\": 0,\n  \"unknown\": true,",
        1,
    );
    assert!(matches!(
        parse_settings(unknown.as_bytes()),
        Err(AppearanceDocumentError::InvalidJson)
    ));
}

#[test]
fn invalid_bounds_and_protocol_alpha_are_rejected() {
    let mut document = AppearanceDocument::default();
    document.preferences.chrome.typography.base_size = f32::NAN;
    assert!(matches!(
        export_settings(&document),
        Err(AppearanceDocumentError::InvalidPreferences)
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
    let document = AppearanceDocument {
        custom_schemes: vec![custom],
        ..AppearanceDocument::default()
    };
    assert!(matches!(
        export_settings(&document),
        Err(AppearanceDocumentError::InvalidCatalog)
    ));
}

#[test]
fn catalog_batch_install_is_atomic_and_revision_checked() {
    let scheme = CustomScheme::Chrome(Box::new(ChromeScheme {
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
    let bytes = include_bytes!("../../third_party/vague-pro-zed/themes/vague-pro.json");
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
    assert!(first[0].id().as_str().ends_with(".0.chrome"));
    assert!(first[1].id().as_str().ends_with(".0.terminal"));

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
                SchemeId::builtin("builtin.vague-pro.chrome.dark"),
            ),
            (
                SchemeKind::Terminal,
                SchemeId::builtin("builtin.spaceterm.terminal.light"),
            ),
        ],
    )
    .unwrap();
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
