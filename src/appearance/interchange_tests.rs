use super::*;
use std::collections::BTreeSet;

fn selected(catalog: &SchemeCatalog, id: SchemeId, appearance: Appearance) -> ResolvedAppearance {
    let mut preferences = AppearancePreferences {
        mode: appearance.into(),
        ..Default::default()
    };
    preferences.terminal.schemes.set(appearance, id);
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
        let terminal = super::builtin::fallback_id(appearance);
        let mut preferences = AppearancePreferences {
            mode: appearance.into(),
            ..Default::default()
        };
        preferences
            .terminal
            .schemes
            .set(appearance, terminal.clone());
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
        let encoded = export_resolved_schemes(&catalog, &before).unwrap();
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
            .terminal
            .schemes
            .set(appearance, installed[0].clone());
        let after = fresh
            .resolve(
                AppearanceGeneration::INITIAL,
                &preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap();
        assert_eq!(before.terminal.colors, after.terminal.colors);
    }
}

#[test]
fn builtin_definition_exports_install_as_portable_copies() {
    let catalog = SchemeCatalog::default();
    for appearance in [Appearance::Light, Appearance::Dark] {
        let id = super::builtin::fallback_id(appearance);
        let encoded = export_schemes(&catalog, std::slice::from_ref(&id)).unwrap();
        let parsed = parse_color_document(encoded.as_bytes()).unwrap();
        let mut fresh = SchemeCatalog::default();
        fresh
            .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
            .unwrap();
        assert_eq!(
            selected(&fresh, parsed.schemes[0].id.clone(), appearance)
                .terminal
                .colors,
            selected(&catalog, id, appearance).terminal.colors
        );
    }
}

#[test]
fn zed_identity_survives_formatting_reordering_and_color_updates_without_authorizing_replacement() {
    let candidate = serde_json::json!({"name":"Ocean","appearance":"dark","style":{"terminal.background":"#010203","terminal.foreground":"#fefefe"}});
    let other = serde_json::json!({"name":"Elsewhere","appearance":"light","style":{}});
    let root = serde_json::json!({"name":"Family","author":"Theme author","themes":[candidate.clone(),other.clone()]});
    let compact = serde_json::to_vec(&root).unwrap();
    let first = [import_zed(&compact, 0).unwrap()];
    let reordered =
        serde_json::json!({"name":"Family","author":"Theme author","themes":[other,candidate]});
    let reordered = serde_json::to_vec_pretty(&reordered).unwrap();
    let second = [import_zed(&reordered, 1).unwrap()];
    assert_eq!(first, second);
    let mut changed: serde_json::Value = serde_json::from_slice(&compact).unwrap();
    changed["themes"][0]["style"]["terminal.background"] = serde_json::json!("#112233");
    let changed = [import_zed(&serde_json::to_vec(&changed).unwrap(), 0).unwrap()];
    assert_eq!(first[0].id, changed[0].id);
    let (old, new) = (&first[0], &changed[0]);
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
fn one_entry_zed_ansi_palette_stays_sparse_through_export_and_reinstall() {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "themes": [{
            "name": "Sparse ANSI",
            "appearance": "dark",
            "style": { "terminal.ansi.red": "#dd1133" }
        }]
    }))
    .unwrap();
    let imported = [import_zed(&bytes, 0).unwrap()];
    let imported_scheme = &imported[0];
    let authored = imported_scheme.colors.normal.as_ref().unwrap();
    assert_eq!(authored.get(1), Some(Color::rgb(0xdd1133)));
    assert_eq!(authored.get(0), None);
    assert!(imported_scheme.colors.bright.is_none());
    assert!(imported_scheme.colors.dim.is_none());

    let catalog = SchemeCatalog::from_color_schemes(&imported).unwrap();
    let encoded = export_schemes(&catalog, std::slice::from_ref(&imported_scheme.id)).unwrap();
    let parsed = parse_color_document(encoded.as_bytes()).unwrap();
    assert_eq!(parsed.schemes, imported.to_vec());

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
    let absent=br##"{"themes":[{"name":"Sparse","appearance":"dark","style":{"terminal.background":"#010203","terminal.foreground":"#fefefe"}}]}"##;
    let null=br##"{"themes":[{"name":"Sparse","appearance":"dark","style":{"terminal.background":"#010203","terminal.foreground":"#fefefe","terminal.ansi.red":null,"future.role":{"opaque":false}}}]}"##;
    let a = [import_zed(absent, 0).unwrap()];
    let b = [import_zed(null, 0).unwrap()];
    let (a, b) = (&a[0], &b[0]);
    assert_eq!(a.colors, b.colors);
    for invalid in ["\"invented\"", "1", "{}"] {
        let value = format!(
            r##"{{"themes":[{{"name":"Bad","appearance":"dark","style":{{"terminal.foreground":{invalid}}}}}]}}"##
        );
        assert!(import_zed(value.as_bytes(), 0).is_err());
    }
}

#[test]
fn snapshot_export_does_not_apply_unused_builtin_overrides_to_missing_request_fallback() {
    let catalog = SchemeCatalog::default();
    let fallback = super::builtin::fallback_id(Appearance::Dark);
    let mut preferences = AppearancePreferences::default();
    preferences.terminal.schemes.dark = SchemeId::new("missing.terminal").unwrap();
    preferences.terminal.overrides.insert(
        fallback.clone(),
        TerminalColorOverrides {
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
    assert_ne!(live.terminal.colors.background, Color::rgb(0xff0000));
    let exported = super::document::export_resolved_schemes(&catalog, &live).unwrap();
    let parsed = parse_color_document(exported.as_bytes()).unwrap();
    let mut fresh = SchemeCatalog::default();
    let ids = fresh
        .install_batch(&parsed.schemes, fresh.revision(), &BTreeSet::new())
        .unwrap();
    assert_eq!(
        selected(&fresh, ids[0].clone(), Appearance::Dark)
            .terminal
            .colors,
        live.terminal.colors
    );
}
