use super::compiler::{ColorProvenance, compile_chrome};
use super::document::export_effective_schemes;
use super::*;
use std::collections::BTreeSet;

fn selected(catalog: &SchemeCatalog, id: SchemeId, appearance: Appearance) -> ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode: appearance.into(),
        ..Default::default()
    };
    preferences.chrome.schemes.set(appearance, id);
    catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap()
}

#[test]
fn effective_exports_install_fresh_and_reproduce_builtin_paints_and_overrides() {
    for appearance in [Appearance::Light, Appearance::Dark] {
        let catalog = SchemeCatalog::default();
        let chrome = super::builtin::fallback_id(SchemeKind::Chrome, appearance);
        let terminal = super::builtin::fallback_id(SchemeKind::Terminal, appearance);
        let mut preferences = AppearancePreferences {
            mode: appearance.into(),
            ..Default::default()
        };
        preferences.chrome.schemes.set(appearance, chrome.clone());
        preferences
            .terminal
            .schemes
            .set(appearance, terminal.clone());
        preferences.chrome.overrides.insert(
            chrome.clone(),
            ChromeColorOverrides {
                background: Some(Color::rgb(0x123456)),
                primary_background: Some(Color::rgba(0xaabbcc88)),
                ..Default::default()
            },
        );
        preferences.terminal.overrides.insert(
            terminal.clone(),
            TerminalColorOverrides {
                cursor: Some(Color::rgb(0xfedcba)),
                ..Default::default()
            },
        );
        let before = catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        let encoded = export_effective_schemes(
            &catalog,
            &preferences,
            &[
                (SchemeKind::Chrome, chrome),
                (SchemeKind::Terminal, terminal),
            ],
        )
        .unwrap();
        let parsed = parse_color_document(encoded.as_bytes()).unwrap();
        let mut fresh = SchemeCatalog::default();
        let installed = fresh
            .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
            .unwrap();
        assert!(installed.iter().all(|id| !id.is_reserved()));
        let mut preferences = AppearancePreferences {
            mode: appearance.into(),
            ..Default::default()
        };
        preferences
            .chrome
            .schemes
            .set(appearance, installed[0].clone());
        preferences
            .terminal
            .schemes
            .set(appearance, installed[1].clone());
        let after = fresh
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        assert_eq!(before.chrome.colors, after.chrome.colors);
        assert_eq!(before.terminal.colors, after.terminal.colors);
    }
}

#[test]
fn retired_override_collisions_merge_by_field_with_current_ids_winning() {
    let palette = |entries: &[(usize, Color)]| {
        let mut colors = [None; 8];
        for &(index, color) in entries {
            colors[index] = Some(color);
        }
        super::scheme::TerminalPaletteOverrides::sparse(colors).unwrap()
    };

    let mut document = AppearanceDocument::default();
    let retired_chrome = SchemeId::builtin("builtin.vague-pro.chrome.dark");
    let current_chrome = super::builtin::dark_chrome_id();
    document.preferences.chrome.overrides.insert(
        retired_chrome.clone(),
        ChromeColorOverrides {
            background: Some(Color::rgb(0x101112)),
            text: Some(Color::rgb(0x202122)),
            ..Default::default()
        },
    );
    document.preferences.chrome.overrides.insert(
        current_chrome.clone(),
        ChromeColorOverrides {
            text: Some(Color::rgb(0x303132)),
            border: Some(Color::rgb(0x404142)),
            ..Default::default()
        },
    );

    let retired_terminal = SchemeId::builtin("builtin.vague-pro.terminal.dark");
    let current_terminal = super::builtin::dark_terminal_id();
    document.preferences.terminal.overrides.insert(
        retired_terminal.clone(),
        TerminalColorOverrides {
            foreground: Some(Color::rgb(0x505152)),
            normal: Some(palette(&[
                (0, Color::rgb(0x000001)),
                (1, Color::rgb(0x010101)),
            ])),
            bright: Some(palette(&[
                (2, Color::rgb(0x020202)),
                (3, Color::rgb(0x030303)),
            ])),
            dim: Some(palette(&[
                (4, Color::rgb(0x040404)),
                (5, Color::rgb(0x050505)),
            ])),
            cursor: Some(Color::rgb(0x606162)),
            cursor_text: OptionalColorOverride::None,
            selection_foreground: OptionalColorOverride::Color(Color::rgb(0x707172)),
            ..Default::default()
        },
    );
    document.preferences.terminal.overrides.insert(
        current_terminal.clone(),
        TerminalColorOverrides {
            background: Some(Color::rgb(0x808182)),
            normal: Some(palette(&[
                (1, Color::rgb(0x111111)),
                (6, Color::rgb(0x161616)),
            ])),
            bright: Some(palette(&[
                (3, Color::rgb(0x131313)),
                (7, Color::rgb(0x171717)),
            ])),
            dim: Some(palette(&[
                (0, Color::rgb(0x101010)),
                (5, Color::rgb(0x151515)),
            ])),
            cursor: Some(Color::rgb(0x909192)),
            selection_foreground: OptionalColorOverride::None,
            ..Default::default()
        },
    );

    let loaded = parse_settings(&serde_json::to_vec(&document).unwrap()).unwrap();

    assert!(
        !loaded
            .preferences
            .chrome
            .overrides
            .contains_key(&retired_chrome)
    );
    let chrome = &loaded.preferences.chrome.overrides[&current_chrome];
    assert_eq!(chrome.background, Some(Color::rgb(0x101112)));
    assert_eq!(chrome.text, Some(Color::rgb(0x303132)));
    assert_eq!(chrome.border, Some(Color::rgb(0x404142)));

    assert!(
        !loaded
            .preferences
            .terminal
            .overrides
            .contains_key(&retired_terminal)
    );
    let terminal = &loaded.preferences.terminal.overrides[&current_terminal];
    assert_eq!(terminal.foreground, Some(Color::rgb(0x505152)));
    assert_eq!(terminal.background, Some(Color::rgb(0x808182)));
    assert_eq!(terminal.cursor, Some(Color::rgb(0x909192)));
    assert_eq!(terminal.cursor_text, OptionalColorOverride::None);
    assert_eq!(terminal.selection_foreground, OptionalColorOverride::None);
    let normal = terminal.normal.as_ref().unwrap();
    assert_eq!(normal.get(0), Some(Color::rgb(0x000001)));
    assert_eq!(normal.get(1), Some(Color::rgb(0x111111)));
    assert_eq!(normal.get(6), Some(Color::rgb(0x161616)));
    let bright = terminal.bright.as_ref().unwrap();
    assert_eq!(bright.get(2), Some(Color::rgb(0x020202)));
    assert_eq!(bright.get(3), Some(Color::rgb(0x131313)));
    assert_eq!(bright.get(7), Some(Color::rgb(0x171717)));
    let dim = terminal.dim.as_ref().unwrap();
    assert_eq!(dim.get(0), Some(Color::rgb(0x101010)));
    assert_eq!(dim.get(4), Some(Color::rgb(0x040404)));
    assert_eq!(dim.get(5), Some(Color::rgb(0x151515)));
}

#[test]
fn resolved_exports_keep_authored_backdrop_metadata_outside_application_preferences() {
    let catalog = SchemeCatalog::default();
    let mut preferences = AppearancePreferences::default();
    preferences.background.blur = true;
    let resolved = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(
        resolved.chrome.composition.requested,
        WindowBackgroundAppearance::Blurred
    );
    let encoded = super::document::export_resolved_schemes(&catalog, &resolved).unwrap();
    let parsed = parse_color_document(encoded.as_bytes()).unwrap();
    let CustomScheme::Chrome(exported) = &parsed.schemes[0] else {
        panic!("resolved export must put Chrome first")
    };
    assert_eq!(exported.window_background, None);

    let authored = parse_color_document(
        br##"{"schema_version":1,"schemes":[{"kind":"chrome","window_background":"blurred","id":"custom.authored-backdrop","name":"Authored Backdrop","appearance":"dark","description":"Source intent","colors":{}}]}"##,
    )
    .unwrap();
    let authored_id = authored.schemes[0].id().clone();
    let authored_catalog = SchemeCatalog::from_custom_schemes(&authored.schemes).unwrap();
    let mut preferences = AppearancePreferences::default();
    preferences.background.blur = false;
    preferences
        .chrome
        .schemes
        .set(Appearance::Dark, authored_id);
    let resolved = authored_catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(
        resolved.chrome.composition.requested,
        WindowBackgroundAppearance::Transparent
    );
    let encoded = super::document::export_resolved_schemes(&authored_catalog, &resolved).unwrap();
    let parsed = parse_color_document(encoded.as_bytes()).unwrap();
    let CustomScheme::Chrome(exported) = &parsed.schemes[0] else {
        panic!("resolved export must put Chrome first")
    };
    assert_eq!(
        exported.window_background,
        Some(WindowBackgroundAppearance::Blurred)
    );
    assert_eq!(
        exported.metadata.description.as_deref(),
        Some("Source intent")
    );
}

#[test]
fn definition_exports_preserve_sparse_authored_intent_and_install_builtin_copies() {
    let catalog = SchemeCatalog::default();
    let id = super::builtin::fallback_id(SchemeKind::Chrome, Appearance::Dark);
    let source = catalog.chrome(&id).unwrap();
    let encoded = export_schemes(&catalog, &[(SchemeKind::Chrome, id.clone())]).unwrap();
    let parsed = parse_color_document(encoded.as_bytes()).unwrap();
    let CustomScheme::Chrome(definition) = &parsed.schemes[0] else {
        panic!()
    };
    assert_eq!(definition.colors, source.colors);
    assert!(definition.colors.modal_scrim.is_none());
    let mut fresh = SchemeCatalog::default();
    fresh
        .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
        .unwrap();
    assert_eq!(
        selected(&fresh, definition.id.clone(), Appearance::Dark)
            .chrome
            .colors,
        selected(&catalog, id, Appearance::Dark).chrome.colors
    );
}

#[test]
fn zed_identity_survives_formatting_reordering_and_color_updates_without_authorizing_replacement() {
    let candidate = serde_json::json!({"name":"Ocean","appearance":"dark","style":{"background":"#010203","text":"#fefefe"}});
    let other = serde_json::json!({"name":"Elsewhere","appearance":"light","style":{}});
    let root = serde_json::json!({"name":"Family","author":"Theme author","themes":[candidate.clone(),other.clone()]});
    let compact = serde_json::to_vec(&root).unwrap();
    let first = import_zed(&compact, 0, &[ZedImportKind::Chrome]).unwrap();
    let reordered =
        serde_json::json!({"name":"Family","author":"Theme author","themes":[other,candidate]});
    let reordered = serde_json::to_vec_pretty(&reordered).unwrap();
    let second = import_zed(&reordered, 1, &[ZedImportKind::Chrome]).unwrap();
    assert_eq!(first, second);
    let mut changed: serde_json::Value = serde_json::from_slice(&compact).unwrap();
    changed["themes"][0]["style"]["background"] = serde_json::json!("#112233");
    let changed = import_zed(
        &serde_json::to_vec(&changed).unwrap(),
        0,
        &[ZedImportKind::Chrome],
    )
    .unwrap();
    assert_eq!(first[0].id(), changed[0].id());
    let (CustomScheme::Chrome(old), CustomScheme::Chrome(new)) = (&first[0], &changed[0]) else {
        panic!()
    };
    assert_eq!(old.metadata.author.as_deref(), Some("Theme author"));
    assert_ne!(
        old.metadata.origin.as_ref().unwrap().fingerprint,
        new.metadata.origin.as_ref().unwrap().fingerprint
    );
    let mut catalog = SchemeCatalog::default();
    catalog
        .install_batch(&first, catalog.revision(), &BTreeSet::new())
        .unwrap();
    assert_eq!(
        catalog.install_batch(&changed, catalog.revision(), &BTreeSet::new()),
        Err(CatalogError::DuplicateId)
    );
    catalog
        .install_batch(
            &changed,
            catalog.revision(),
            &BTreeSet::from([old.id.clone()]),
        )
        .unwrap();
}

#[test]
fn sparse_zed_and_native_intent_compile_identically_after_overrides() {
    for appearance in ["light", "dark"] {
        let bytes=serde_json::to_vec(&serde_json::json!({"themes":[{"name":"Sparse","appearance":appearance,"style":{"background":"#010203","text":"#fefefe","element.selected":"#aa00aa","error":"#dd1133","background.appearance":"blurred","scrollbar.track.background":"#12233444","scrollbar.thumb.active_background":"#ffff00"}}]})).unwrap();
        let schemes = import_zed(&bytes, 0, &[ZedImportKind::Chrome]).unwrap();
        let CustomScheme::Chrome(scheme) = &schemes[0] else {
            panic!()
        };
        assert!(scheme.colors.modal_scrim.is_none());
        let native = ChromeColorOverrides {
            background: Some(Color::rgb(0x010203)),
            text: Some(Color::rgb(0xfefefe)),
            element_selected: Some(Color::rgb(0xaa00aa)),
            error: Some(Color::rgb(0xdd1133)),
            scrollbar_track: Some(Color::rgba(0x12233444)),
            scrollbar_thumb_active_background: Some(Color::rgb(0xffff00)),
            ..Default::default()
        };
        let overrides = ChromeColorOverrides {
            background: Some(Color::rgb(0x334455)),
            ..Default::default()
        };
        assert_eq!(
            compile_chrome(scheme.appearance, &scheme.colors, &overrides),
            compile_chrome(scheme.appearance, &native, &overrides)
        );
        let catalog = SchemeCatalog::from_custom_schemes(&schemes).unwrap();
        let resolved = selected(&catalog, scheme.id.clone(), scheme.appearance);
        assert_eq!(
            resolved.chrome.composition.requested,
            WindowBackgroundAppearance::Blurred
        );
        assert_eq!(
            resolved.chrome.composition.effective,
            WindowBackgroundAppearance::Opaque
        );
        assert_eq!(
            resolved.chrome.colors.scrollbar_thumb_active_background,
            Color::rgb(0xffff00)
        );
        assert!(
            resolved
                .chrome
                .colors
                .selection_foreground
                .contrast_ratio(resolved.chrome.colors.selection_background)
                >= 4.5
        );
        let exported =
            export_schemes(&catalog, &[(SchemeKind::Chrome, scheme.id.clone())]).unwrap();
        assert_eq!(
            parse_color_document(exported.as_bytes()).unwrap().schemes,
            schemes
        );
    }
}

#[test]
fn zed_selected_hover_keeps_the_selected_surface_when_hover_differs() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "themes": [{
            "name": "Distinct interaction states",
            "appearance": "dark",
            "style": {
                "ghost_element.hover": "#112233",
                "ghost_element.selected": "#aabbcc"
            }
        }]
    }))
    .unwrap();

    let imported = import_zed(&bytes, 0, &[ZedImportKind::Chrome]).unwrap();
    let CustomScheme::Chrome(scheme) = &imported[0] else {
        panic!()
    };

    assert!(scheme.colors.row_selected_hover_background.is_none());
    let compiled = compile_chrome(scheme.appearance, &scheme.colors, &Default::default());
    assert_eq!(
        compiled.colors.row_selected_hover_background,
        compiled
            .colors
            .row_selected_background
            .mix(compiled.colors.text, 0.06)
    );
    assert_ne!(
        compiled.colors.row_selected_hover_background,
        compiled.colors.row_hover_background
    );
    assert_eq!(
        compiled.provenance["row_selected_hover_background"],
        ColorProvenance::Derived
    );
}

#[test]
fn one_entry_zed_ansi_palette_stays_sparse_through_export_and_reinstall() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "themes": [{
            "name": "Sparse ANSI",
            "appearance": "dark",
            "style": { "terminal.ansi.red": "#dd1133" }
        }]
    }))
    .unwrap();
    let imported = import_zed(&bytes, 0, &[ZedImportKind::Terminal]).unwrap();
    let CustomScheme::Terminal(imported_scheme) = &imported[0] else {
        panic!()
    };
    let authored = imported_scheme.colors.normal.as_ref().unwrap();
    assert_eq!(authored.get(1), Some(Color::rgb(0xdd1133)));
    assert_eq!(authored.get(0), None);
    assert!(imported_scheme.colors.bright.is_none());
    assert!(imported_scheme.colors.dim.is_none());

    let catalog = SchemeCatalog::from_custom_schemes(&imported).unwrap();
    let encoded = export_schemes(
        &catalog,
        &[(SchemeKind::Terminal, imported_scheme.id.clone())],
    )
    .unwrap();
    let parsed = parse_color_document(encoded.as_bytes()).unwrap();
    assert_eq!(parsed.schemes, imported);

    let mut fresh = SchemeCatalog::default();
    let installed = fresh
        .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
        .unwrap();
    let mut preferences = AppearancePreferences {
        mode: Appearance::Dark.into(),
        ..Default::default()
    };
    preferences
        .terminal
        .schemes
        .set(Appearance::Dark, installed[0].clone());
    let resolved = fresh
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_eq!(resolved.terminal.colors.normal[1], Color::rgb(0xdd1133));
    assert_eq!(
        resolved.terminal.colors.normal[0],
        super::builtin::terminal_base(Appearance::Dark).normal[0]
    );
}

#[test]
fn zed_null_missing_unknown_and_invalid_inputs_have_bounded_behavior() {
    let absent=br##"{"themes":[{"name":"Sparse","appearance":"dark","style":{"background":"#010203","text":"#fefefe"}}]}"##;
    let null=br##"{"themes":[{"name":"Sparse","appearance":"dark","style":{"background":"#010203","text":"#fefefe","element.selected":null,"future.role":{"opaque":false}}}]}"##;
    let a = import_zed(absent, 0, &[ZedImportKind::Chrome]).unwrap();
    let b = import_zed(null, 0, &[ZedImportKind::Chrome]).unwrap();
    let (CustomScheme::Chrome(a), CustomScheme::Chrome(b)) = (&a[0], &b[0]) else {
        panic!()
    };
    assert_eq!(a.colors, b.colors);
    for invalid in ["\"invented\"", "1", "{}"] {
        let value = format!(
            r##"{{"themes":[{{"name":"Bad","appearance":"dark","style":{{"background.appearance":{invalid}}}}}]}}"##
        );
        assert!(import_zed(value.as_bytes(), 0, &[ZedImportKind::Chrome]).is_err());
    }
}

#[test]
fn pinned_upstream_roles_cross_the_production_importer_and_compiler() {
    let bytes = include_bytes!("fixtures/vague-pro/theme.json");
    let candidates = list_zed_candidates(bytes).unwrap();
    let candidate = candidates
        .iter()
        .find(|value| value.name == "Vague Pro")
        .unwrap();
    let source: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let style = source["themes"][candidate.index]["style"]
        .as_object()
        .unwrap();
    for prefix in [
        "terminal.ansi.",
        "terminal.ansi.bright_",
        "terminal.ansi.dim_",
    ] {
        for name in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ] {
            let key = format!("{prefix}{name}");
            assert!(
                style.get(&key).is_some_and(serde_json::Value::is_string),
                "pinned upstream omitted {key}"
            );
        }
    }
    for key in [
        "terminal.foreground",
        "terminal.background",
        "terminal.bright_foreground",
        "terminal.dim_foreground",
        "background",
        "text",
        "border",
        "element.selected",
        "ghost_element.hover",
        "ghost_element.selected",
        "scrollbar.thumb.background",
        "scrollbar.thumb.hover_background",
    ] {
        assert!(
            style.get(key).is_some_and(serde_json::Value::is_string),
            "pinned upstream omitted {key}"
        );
    }
    let imported = import_zed(
        bytes,
        candidate.index,
        &[ZedImportKind::Chrome, ZedImportKind::Terminal],
    )
    .unwrap();
    let CustomScheme::Chrome(chrome) = &imported[0] else {
        panic!()
    };
    let compiled = compile_chrome(chrome.appearance, &chrome.colors, &Default::default());
    assert_eq!(compiled.colors.background, Color::rgb(0x141415));
    assert_eq!(compiled.colors.text, Color::rgb(0xcdcdcd));
    assert_eq!(compiled.colors.input_background, compiled.colors.background);
    assert_eq!(compiled.colors.modal_scrim, Color::rgba(0x14141599));
    assert!(
        compiled
            .colors
            .input_placeholder
            .contrast_ratio(compiled.colors.input_background)
            >= 4.5
    );
    let CustomScheme::Terminal(terminal) = &imported[1] else {
        panic!()
    };
    for (prefix, palette) in [
        ("terminal.ansi.", terminal.colors.normal.as_ref().unwrap()),
        (
            "terminal.ansi.bright_",
            terminal.colors.bright.as_ref().unwrap(),
        ),
        ("terminal.ansi.dim_", terminal.colors.dim.as_ref().unwrap()),
    ] {
        for (index, name) in [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ]
        .into_iter()
        .enumerate()
        {
            let key = format!("{prefix}{name}");
            let expected = Color::parse(style[&key].as_str().unwrap()).unwrap();
            assert_eq!(palette.get(index), Some(expected));
        }
    }
}

#[test]
fn snapshot_export_does_not_apply_unused_builtin_overrides_to_missing_request_fallback() {
    let catalog = SchemeCatalog::default();
    let fallback = super::builtin::fallback_id(SchemeKind::Chrome, Appearance::Dark);
    let mut preferences = AppearancePreferences::default();
    preferences.chrome.schemes.dark = SchemeId::new("missing.chrome").unwrap();
    preferences.chrome.overrides.insert(
        fallback.clone(),
        ChromeColorOverrides {
            background: Some(Color::rgb(0xff0000)),
            ..Default::default()
        },
    );
    let live = catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            &preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap();
    assert_ne!(live.chrome.colors.background, Color::rgb(0xff0000));
    let exported = super::document::export_resolved_schemes(&catalog, &live).unwrap();
    let parsed = parse_color_document(exported.as_bytes()).unwrap();
    let mut fresh = SchemeCatalog::default();
    let ids = fresh
        .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
        .unwrap();
    assert_eq!(
        selected(&fresh, ids[0].clone(), Appearance::Dark)
            .chrome
            .colors,
        live.chrome.colors
    );
}

#[test]
fn native_tab_separator_survives_export_and_reinstall_apart_from_outlined_controls() {
    let bytes = br##"{"schema_version":1,"schemes":[{"kind":"chrome","id":"test.tab-separator","name":"Tab separator","appearance":"dark","colors":{"background":"#101114","text":"#e6e7ea","outline_border":"#ffffff","tab_separator":"#3a3d44"}}]}"##;
    let parsed = parse_color_document(bytes).unwrap();
    let CustomScheme::Chrome(scheme) = &parsed.schemes[0] else {
        panic!()
    };
    assert_eq!(scheme.colors.tab_separator, Some(Color::rgb(0x3a3d44)));
    let catalog = SchemeCatalog::from_custom_schemes(&parsed.schemes).unwrap();
    let before = selected(&catalog, scheme.id.clone(), Appearance::Dark);
    assert_eq!(before.chrome.colors.tab_separator, Color::rgb(0x3a3d44));
    assert_eq!(before.chrome.colors.outline_border, Color::rgb(0xffffff));
    assert_eq!(
        before.chrome.provenance["tab_separator"],
        ColorProvenance::Authored
    );

    let exported = export_schemes(&catalog, &[(SchemeKind::Chrome, scheme.id.clone())]).unwrap();
    let reparsed = parse_color_document(exported.as_bytes()).unwrap();
    assert_eq!(reparsed.schemes, parsed.schemes);
    let mut fresh = SchemeCatalog::default();
    let installed = fresh
        .install_batch(&reparsed.schemes, fresh.revision(), &BTreeSet::new())
        .unwrap();
    assert_eq!(
        selected(&fresh, installed[0].clone(), Appearance::Dark)
            .chrome
            .colors,
        before.chrome.colors
    );
}
