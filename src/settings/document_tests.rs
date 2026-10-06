use super::*;
use crate::appearance::{
    Appearance, AppearanceGeneration, AppearanceMode, AppearancePreferences, AvailableFonts,
    ChromeDensity, Color, OptionalColorOverride, ResetTarget, ResolutionError, SystemAppearance,
    TerminalColorOverrides, TerminalColors, TerminalFontFamily, ThemeMetadata,
};

#[test]
fn legacy_transparency_exports_as_opacity() {
    for (transparency, expected_opacity) in [(0.0, 1.0), (0.35, 0.65), (1.0, 0.0)] {
        let legacy = legacy_document(
            &SettingsDocument::default(),
            serde_json::json!(transparency),
        );

        let document = parse_settings(&serde_json::to_vec(&legacy).unwrap()).unwrap();
        let exported: serde_json::Value =
            serde_json::from_str(&export_settings(&document).unwrap()).unwrap();
        let opacity = exported["appearance"]["window"]["opacity"]
            .as_f64()
            .unwrap();
        assert!((opacity - expected_opacity).abs() < 0.000001);
        assert_eq!(exported["schema_version"], 4);
        assert!(
            exported["appearance"]["window"]
                .get("transparency")
                .is_none()
        );
        assert_eq!(
            parse_settings(&serde_json::to_vec(&exported).unwrap()).unwrap(),
            document
        );
    }
}

fn legacy_document(
    document: &SettingsDocument,
    transparency: serde_json::Value,
) -> serde_json::Value {
    let mut legacy = serde_json::to_value(document).unwrap();
    legacy["schema_version"] = serde_json::json!(3);
    let window = legacy["appearance"]["window"].as_object_mut().unwrap();
    window.remove("opacity");
    window.insert("transparency".into(), transparency);
    legacy
}

#[test]
fn legacy_migration_preserves_revision_and_every_other_setting() {
    let mut expected = reset_fixture();
    expected.revision = 37;
    expected.appearance.window.opacity = 0.75;
    expected.clipboard.allow_read = true;
    expected.updates.automatic_downloads = false;
    expected.keybindings = serde_json::from_value(serde_json::json!({"close_tab": null})).unwrap();
    let legacy = legacy_document(&expected, serde_json::json!(0.25));

    assert_eq!(
        parse_settings(&serde_json::to_vec(&legacy).unwrap()).unwrap(),
        expected
    );
}

#[test]
fn legacy_migration_rejects_invalid_transparency_values() {
    for transparency in [
        serde_json::json!(-0.01),
        serde_json::json!(1.01),
        serde_json::json!(1e100),
    ] {
        let legacy = legacy_document(&SettingsDocument::default(), transparency);
        assert_eq!(
            parse_settings(&serde_json::to_vec(&legacy).unwrap()),
            Err(SettingsDocumentError::InvalidAppearance)
        );
    }
    for transparency in [
        serde_json::Value::Null,
        serde_json::json!("0.35"),
        serde_json::json!(true),
        serde_json::json!([]),
    ] {
        let legacy = legacy_document(&SettingsDocument::default(), transparency);
        assert_eq!(
            parse_settings(&serde_json::to_vec(&legacy).unwrap()),
            Err(SettingsDocumentError::InvalidJson)
        );
    }
}

#[test]
fn settings_versions_reject_ambiguous_or_mismatched_window_fields() {
    let canonical = serde_json::to_value(SettingsDocument::default()).unwrap();
    let legacy = legacy_document(&SettingsDocument::default(), serde_json::json!(0.35));
    let mut legacy_with_opacity = legacy.clone();
    legacy_with_opacity["appearance"]["window"]["opacity"] = serde_json::json!(0.65);
    let mut legacy_without_transparency = legacy.clone();
    legacy_without_transparency["appearance"]["window"]
        .as_object_mut()
        .unwrap()
        .remove("transparency");
    let mut legacy_with_canonical_fields = canonical.clone();
    legacy_with_canonical_fields["schema_version"] = serde_json::json!(3);
    let mut canonical_with_legacy_fields = legacy;
    canonical_with_legacy_fields["schema_version"] = serde_json::json!(4);
    let mut canonical_with_transparency = canonical;
    canonical_with_transparency["appearance"]["window"]["transparency"] = serde_json::json!(0.35);

    for value in [
        legacy_with_opacity,
        legacy_without_transparency,
        legacy_with_canonical_fields,
        canonical_with_legacy_fields,
        canonical_with_transparency,
    ] {
        assert_eq!(
            parse_settings(&serde_json::to_vec(&value).unwrap()),
            Err(SettingsDocumentError::InvalidJson)
        );
    }
}

#[test]
fn legacy_migration_keeps_duplicate_and_unknown_field_rejection() {
    let mut legacy = legacy_document(&SettingsDocument::default(), serde_json::json!(0.35));
    let encoded = serde_json::to_string(&legacy).unwrap();
    let duplicate = encoded.replacen(
        "\"transparency\":0.35",
        "\"transparency\":0.35,\"transparency\":0.7",
        1,
    );
    assert_eq!(
        parse_settings(duplicate.as_bytes()),
        Err(SettingsDocumentError::DuplicateKey)
    );

    legacy["appearance"]["window"]["unknown"] = serde_json::json!(true);
    assert_eq!(
        parse_settings(&serde_json::to_vec(&legacy).unwrap()),
        Err(SettingsDocumentError::InvalidJson)
    );
}

#[test]
fn a_terminal_theme_must_match_its_slot_appearance() {
    let mut document = SettingsDocument::default();
    document.appearance.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.dark");
    document.appearance.mode = AppearanceMode::Light;
    assert_eq!(
        document.validate(),
        Err(SettingsDocumentError::InvalidAppearance)
    );
    assert!(matches!(
        ThemeCatalog::default().resolve(
            AppearanceGeneration::INITIAL,
            &document.appearance,
            SystemAppearance::unavailable(),
            &AvailableFonts::default()
        ),
        Err(ResolutionError::AppearanceMismatch)
    ));
}

fn reset_fixture() -> SettingsDocument {
    let mut document = SettingsDocument::default();
    document.appearance.mode = AppearanceMode::Light;
    document.appearance.window.density = ChromeDensity::Comfortable;
    document.appearance.window.opacity = 0.2;
    document.appearance.window.blur = false;
    document.appearance.terminal.themes.light = ThemeId::new("custom.reset-light").unwrap();
    document.appearance.terminal.typography.family = TerminalFontFamily::Named {
        family: String::from("Menlo"),
    };
    document.appearance.terminal.typography.base_size = 32.0;
    document.appearance.terminal.typography.regular_weight = 800;
    document.appearance.terminal.typography.bold_weight = 900;
    document.appearance.terminal.typography.line_height = 2.0;
    document.appearance.terminal.typography.italic = false;
    document.appearance.terminal.rendering.bold_as_bright = false;
    document.appearance.terminal.overrides.insert(
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
        (ResetTarget::Opacity, |value| value.window.opacity = 0.65),
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
        let mut expected = actual.appearance.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.appearance, expected, "{target:?}");
        assert_eq!(actual.terminal_themes, retained_themes, "{target:?}");
    }

    assert_ne!(reset_fixture().appearance, defaults);
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
        let mut expected = actual.appearance.clone();
        expected_edit(&mut expected);
        actual.reset(target.clone()).unwrap();
        assert_eq!(actual.appearance, expected, "{target:?}");
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
        document.appearance,
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
    document.appearance.terminal.themes.dark = imported.clone();
    document.validate().unwrap();

    document.reset_all();

    assert_ne!(document.appearance.terminal.themes.dark, imported);
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
        let before_window = actual.appearance.window.clone();
        let mut expected = serde_json::to_value(
            actual
                .appearance
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
                    .appearance
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
            actual.appearance.window, before_window,
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
    sparse.appearance.terminal.overrides.insert(
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
            .appearance
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
    document.appearance.terminal.themes.dark = ThemeId::new("custom.kept-dark").unwrap();
    let encoded = export_settings(&document)
        .unwrap()
        .replace("builtin.spaceterm.light", "custom.removed-light");

    let read = parse_settings(encoded.as_bytes()).unwrap();

    assert_eq!(
        read.appearance.terminal.themes.light,
        ThemeId::builtin("builtin.spaceterm.light")
    );
    assert_eq!(
        read.appearance.terminal.themes.dark,
        ThemeId::new("custom.kept-dark").unwrap()
    );
}

#[test]
fn native_settings_are_canonical_strict_and_round_trip() {
    let mut document = SettingsDocument::default();
    document.appearance.mode = AppearanceMode::Auto;
    document.appearance.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.dark");
    assert!(
        export_settings(&document).is_err(),
        "a slot should name a theme of its own appearance"
    );
    document.appearance.terminal.themes.light = ThemeId::new("missing.terminal.light").unwrap();
    assert!(
        export_settings(&document).is_err(),
        "a slot should name an installed theme"
    );
    document.appearance.terminal.themes.light = ThemeId::builtin("builtin.spaceterm.light");
    let encoded = export_settings(&document).unwrap();
    let encoded_value: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert_eq!(encoded_value["schema_version"], 4);
    assert_eq!(parse_settings(encoded.as_bytes()).unwrap(), document);
    let unsupported = encoded.replacen("\"schema_version\": 4", "\"schema_version\": 2", 1);
    assert!(matches!(
        parse_settings(unsupported.as_bytes()),
        Err(SettingsDocumentError::UnsupportedVersion)
    ));
    assert!(matches!(parse_settings(br#"{"schema_version":3,"schema_version":3,"revision":0,"appearance":{},"terminal_themes":[]}"#),
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
    document.appearance.terminal.typography.base_size = f32::NAN;
    assert!(matches!(
        export_settings(&document),
        Err(SettingsDocumentError::InvalidAppearance)
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
    assert!(encoded.find("\"keybindings\"").unwrap() < encoded.find("\"appearance\"").unwrap());

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
fn replacing_settings_takes_the_imported_catalog_and_keeps_identity() {
    let imported = reset_fixture();
    let mut document = SettingsDocument {
        revision: 4,
        ..SettingsDocument::default()
    };

    document.replace_settings(imported.clone());

    assert_eq!(document.appearance, imported.appearance);
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
