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
    preferences.chrome.schemes.dark = SchemeId::builtin("builtin.vague-pro.terminal.dark");
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
    preferences.chrome.schemes.light = SchemeId::builtin("builtin.vague-pro.chrome.dark");
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
    let mut document = AppearanceDocument::default();
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
        Err(AppearanceDocumentError::UnsupportedVersion)
    ));
    assert!(matches!(parse_settings(br#"{"schema_version":2,"schema_version":2,"revision":0,"preferences":{},"custom_schemes":[]}"#),
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
                SchemeId::builtin("builtin.vague-pro.chrome.dark"),
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
