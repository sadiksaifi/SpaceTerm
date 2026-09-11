use std::collections::BTreeSet;

use super::*;

fn chrome_scheme(id: impl Into<String>, name: impl Into<String>) -> CustomScheme {
    CustomScheme::Chrome(Box::new(ChromeScheme {
        id: SchemeId::new(id).unwrap(),
        name: name.into(),
        appearance: Appearance::Dark,
        metadata: SchemeMetadata::default(),
        colors: ChromeColorOverrides::default(),
    }))
}

fn custom_scheme_count(catalog: &SchemeCatalog) -> usize {
    catalog
        .schemes()
        .iter()
        .filter(|scheme| !scheme.id().is_reserved())
        .count()
}

fn native_document(scheme: &str) -> String {
    format!(r#"{{"schema_version":1,"schemes":[{scheme}]}}"#)
}

#[test]
fn native_import_rejects_explicit_null_for_non_nullable_optional_fields() {
    let schemes = [
        r##"{"kind":"chrome","id":"custom.null-author","name":"Null","appearance":"dark","author":null,"colors":{}}"##,
        r##"{"kind":"chrome","id":"custom.null-chrome","name":"Null","appearance":"dark","colors":{"background":null}}"##,
        r##"{"kind":"terminal","id":"custom.null-terminal","name":"Null","appearance":"dark","colors":{"foreground":null}}"##,
        r##"{"kind":"terminal","id":"custom.null-palette","name":"Null","appearance":"dark","colors":{"normal":null}}"##,
    ];

    for scheme in schemes {
        assert!(
            parse_color_document(native_document(scheme).as_bytes()).is_err(),
            "explicit null was accepted in {scheme}"
        );
    }
}

#[test]
fn native_import_preserves_the_four_explicitly_nullable_terminal_roles() {
    let document = native_document(
        r##"{"kind":"terminal","id":"custom.nullable","name":"Nullable","appearance":"dark","colors":{"cursor_text":null,"selection_foreground":null,"find_match_foreground":null,"find_active_match_foreground":null}}"##,
    );

    let parsed = parse_color_document(document.as_bytes()).unwrap();
    let CustomScheme::Terminal(scheme) = &parsed.schemes[0] else {
        panic!("expected terminal scheme");
    };
    assert_eq!(scheme.colors.cursor_text, OptionalColorOverride::None);
    assert_eq!(
        scheme.colors.selection_foreground,
        OptionalColorOverride::None
    );
    assert_eq!(
        scheme.colors.find_match_foreground,
        OptionalColorOverride::None
    );
    assert_eq!(
        scheme.colors.find_active_match_foreground,
        OptionalColorOverride::None
    );
}

#[test]
fn catalog_rejects_an_empty_batch_without_advancing_revision() {
    let mut catalog = SchemeCatalog::default();

    assert_eq!(
        catalog.install_batch(&[], 0, &BTreeSet::new()),
        Err(CatalogError::EmptyBatch)
    );
    assert_eq!(catalog.revision(), 0);
}

#[test]
fn catalog_replacement_at_capacity_does_not_count_as_an_addition() {
    let schemes = (0..128)
        .map(|index| chrome_scheme(format!("custom.capacity{index}"), format!("Scheme {index}")))
        .collect::<Vec<_>>();
    let mut catalog = SchemeCatalog::from_custom_schemes(&schemes).unwrap();
    let replacement = chrome_scheme("custom.capacity64", "Replaced");
    let replace = BTreeSet::from([replacement.id().clone()]);

    assert_eq!(
        catalog
            .install_batch(std::slice::from_ref(&replacement), 0, &replace)
            .unwrap(),
        vec![replacement.id().clone()]
    );
    assert_eq!(catalog.revision(), 1);
    assert_eq!(custom_scheme_count(&catalog), 128);
    assert_eq!(
        catalog
            .chrome(replacement.id())
            .map(|scheme| scheme.name.as_str()),
        Some("Replaced")
    );
    assert_eq!(
        catalog.install_batch(
            &[chrome_scheme("custom.over-capacity", "Overflow")],
            1,
            &BTreeSet::new()
        ),
        Err(CatalogError::TooManySchemes)
    );
    assert_eq!(catalog.revision(), 1);
}

#[test]
fn catalog_requires_replacement_intent_to_exactly_match_incoming_collisions() {
    let original_a = chrome_scheme("custom.intent-a", "Original A");
    let original_b = chrome_scheme("custom.intent-b", "Original B");
    let mut catalog =
        SchemeCatalog::from_custom_schemes(&[original_a.clone(), original_b.clone()]).unwrap();
    let before = catalog.schemes();

    let replacement_a = chrome_scheme("custom.intent-a", "Replacement A");
    let extra_intent = BTreeSet::from([original_a.id().clone(), original_b.id().clone()]);
    assert_eq!(
        catalog.install_batch(std::slice::from_ref(&replacement_a), 0, &extra_intent),
        Err(CatalogError::UnknownReplacement)
    );

    let new_scheme = chrome_scheme("custom.intent-new", "New");
    let nonexistent_intent = BTreeSet::from([new_scheme.id().clone()]);
    assert_eq!(
        catalog.install_batch(&[new_scheme], 0, &nonexistent_intent),
        Err(CatalogError::UnknownReplacement)
    );

    assert_eq!(
        catalog.install_batch(&[replacement_a], 0, &BTreeSet::new()),
        Err(CatalogError::DuplicateId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.schemes(), before);
}

#[test]
fn catalog_never_replaces_a_reserved_builtin() {
    let mut catalog = SchemeCatalog::default();
    let before = catalog.schemes();
    let reserved = chrome_scheme("builtin.vague-pro.chrome.dark", "Overwrite");
    let replace = BTreeSet::from([reserved.id().clone()]);

    assert_eq!(
        catalog.install_batch(&[reserved], 0, &replace),
        Err(CatalogError::ReservedId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.schemes(), before);
}

#[test]
fn catalog_batch_failure_is_atomic_after_valid_entries() {
    let original = chrome_scheme("custom.atomic-original", "Original");
    let mut catalog = SchemeCatalog::from_custom_schemes(&[original]).unwrap();
    let before = catalog.schemes();
    let batch = [
        chrome_scheme("custom.atomic-new", "New"),
        chrome_scheme("custom.atomic-new", "Duplicate"),
    ];

    assert_eq!(
        catalog.install_batch(&batch, 0, &BTreeSet::new()),
        Err(CatalogError::DuplicateId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.schemes(), before);
}

#[test]
fn catalog_rejects_stale_and_oversized_batches_without_mutation() {
    let first = chrome_scheme("custom.first", "First");
    let mut catalog = SchemeCatalog::default();
    catalog
        .install_batch(&[first], 0, &BTreeSet::new())
        .unwrap();
    let before = catalog.schemes();

    assert_eq!(
        catalog.install_batch(
            &[chrome_scheme("custom.stale", "Stale")],
            0,
            &BTreeSet::new()
        ),
        Err(CatalogError::RevisionConflict)
    );
    let oversized = (0..33)
        .map(|index| chrome_scheme(format!("custom.batch{index}"), format!("Batch {index}")))
        .collect::<Vec<_>>();
    assert_eq!(
        catalog.install_batch(&oversized, 1, &BTreeSet::new()),
        Err(CatalogError::TooManySchemes)
    );
    assert_eq!(catalog.revision(), 1);
    assert_eq!(catalog.schemes(), before);
}

#[test]
fn deterministic_zed_reimport_collides_until_explicitly_replaced() {
    let bytes = include_bytes!("../../third_party/vague-pro-zed/themes/vague-pro.json");
    let imported = import_zed(bytes, 0, &[ZedImportKind::Chrome, ZedImportKind::Terminal]).unwrap();
    let mut catalog = SchemeCatalog::default();
    catalog
        .install_batch(&imported, 0, &BTreeSet::new())
        .unwrap();
    let before = catalog.schemes();

    let reimported =
        import_zed(bytes, 0, &[ZedImportKind::Chrome, ZedImportKind::Terminal]).unwrap();
    assert_eq!(imported, reimported);
    assert_eq!(
        catalog.install_batch(&reimported, 1, &BTreeSet::new()),
        Err(CatalogError::DuplicateId)
    );
    assert_eq!(catalog.revision(), 1);
    assert_eq!(catalog.schemes(), before);

    let replace = reimported
        .iter()
        .map(|scheme| scheme.id().clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        catalog.install_batch(&reimported, 1, &replace).unwrap(),
        replace.iter().cloned().collect::<Vec<_>>()
    );
    assert_eq!(catalog.revision(), 2);
}

#[test]
fn catalog_scheme_listing_is_globally_sorted_and_includes_builtins() {
    let catalog = SchemeCatalog::from_custom_schemes(&[
        chrome_scheme("custom.z-last", "Last"),
        chrome_scheme("custom.a-first", "First"),
    ])
    .unwrap();
    let schemes = catalog.schemes();
    let ids = schemes.iter().map(CustomScheme::id).collect::<Vec<_>>();

    assert_eq!(schemes.len(), 6);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
}
