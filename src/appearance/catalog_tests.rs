use std::collections::BTreeSet;

use super::terminal_theme::MAX_INSTALLED_THEMES;
use super::*;

fn terminal_theme(id: impl Into<String>, name: impl Into<String>) -> TerminalTheme {
    TerminalTheme {
        id: ThemeId::new(id).unwrap(),
        name: name.into(),
        appearance: Appearance::Dark,
        metadata: ThemeMetadata::default(),
        colors: TerminalColorOverrides::default(),
    }
}

fn imported_theme_count(catalog: &ThemeCatalog) -> usize {
    catalog
        .themes()
        .iter()
        .filter(|theme| !theme.id.is_reserved())
        .count()
}

#[test]
fn stored_themes_reject_explicit_null_for_non_nullable_optional_fields() {
    let themes = [
        r##"{"id":"custom.null-author","name":"Null","appearance":"dark","author":null,"colors":{}}"##,
        r##"{"id":"custom.null-background","name":"Null","appearance":"dark","colors":{"background":null}}"##,
        r##"{"id":"custom.null-terminal","name":"Null","appearance":"dark","colors":{"foreground":null}}"##,
        r##"{"id":"custom.null-palette","name":"Null","appearance":"dark","colors":{"normal":null}}"##,
    ];

    for theme in themes {
        assert!(
            serde_json::from_str::<TerminalTheme>(theme).is_err(),
            "explicit null was accepted in {theme}"
        );
    }
}

#[test]
fn stored_themes_preserve_the_four_explicitly_nullable_terminal_roles() {
    let theme: TerminalTheme = serde_json::from_str(
        r##"{"id":"custom.nullable","name":"Nullable","appearance":"dark","colors":{"cursor_text":null,"selection_foreground":null,"find_match_foreground":null,"find_active_match_foreground":null}}"##,
    )
    .unwrap();

    assert_eq!(theme.colors.cursor_text, OptionalColorOverride::None);
    assert_eq!(
        theme.colors.selection_foreground,
        OptionalColorOverride::None
    );
    assert_eq!(
        theme.colors.find_match_foreground,
        OptionalColorOverride::None
    );
    assert_eq!(
        theme.colors.find_active_match_foreground,
        OptionalColorOverride::None
    );
}

#[test]
fn catalog_rejects_an_empty_batch_without_advancing_revision() {
    let mut catalog = ThemeCatalog::default();

    assert_eq!(
        catalog.install_batch(&[], 0, &BTreeSet::new()),
        Err(CatalogError::EmptyBatch)
    );
    assert_eq!(catalog.revision(), 0);
}

#[test]
fn catalog_reinstall_at_capacity_does_not_count_as_an_addition() {
    let themes = (0..MAX_INSTALLED_THEMES)
        .map(|index| terminal_theme(format!("custom.capacity{index}"), format!("Theme {index}")))
        .collect::<Vec<_>>();
    let mut catalog = ThemeCatalog::from_terminal_themes(&themes).unwrap();
    let replacement = terminal_theme("custom.capacity64", "Replaced");

    assert_eq!(
        catalog
            .install_batch(std::slice::from_ref(&replacement), 0, &BTreeSet::new())
            .unwrap(),
        vec![replacement.id.clone()]
    );
    assert_eq!(catalog.revision(), 1);
    assert_eq!(imported_theme_count(&catalog), MAX_INSTALLED_THEMES);
    assert_eq!(
        catalog
            .get(&replacement.id)
            .map(|theme| theme.name.as_str()),
        Some("Replaced")
    );
    assert_eq!(
        catalog.install_batch(
            &[terminal_theme("custom.over-capacity", "Overflow")],
            1,
            &BTreeSet::new()
        ),
        Err(CatalogError::TooManyThemes)
    );
    assert_eq!(catalog.revision(), 1);
}

#[test]
fn catalog_retires_themes_in_the_same_batch_and_frees_their_capacity() {
    let themes = (0..MAX_INSTALLED_THEMES)
        .map(|index| terminal_theme(format!("custom.retire{index}"), format!("Theme {index}")))
        .collect::<Vec<_>>();
    let mut catalog = ThemeCatalog::from_terminal_themes(&themes).unwrap();
    let retired = BTreeSet::from([themes[0].id.clone(), themes[1].id.clone()]);
    let incoming = [terminal_theme("custom.incoming", "Incoming")];

    assert_eq!(
        catalog.install_batch(&incoming, 0, &retired).unwrap(),
        vec![incoming[0].id.clone()]
    );
    assert!(!catalog.contains(&themes[0].id));
    assert!(!catalog.contains(&themes[1].id));
    assert!(catalog.contains(&incoming[0].id));
    assert_eq!(imported_theme_count(&catalog), MAX_INSTALLED_THEMES - 1);
}

#[test]
fn catalog_retires_only_installed_imported_themes() {
    let original = terminal_theme("custom.original", "Original");
    let mut catalog = ThemeCatalog::from_terminal_themes(std::slice::from_ref(&original)).unwrap();
    let before = catalog.themes();
    let incoming = [terminal_theme("custom.incoming", "Incoming")];

    assert_eq!(
        catalog.install_batch(
            &incoming,
            0,
            &BTreeSet::from([ThemeId::new("custom.unknown").unwrap()])
        ),
        Err(CatalogError::UnknownTheme)
    );
    assert_eq!(
        catalog.install_batch(
            &incoming,
            0,
            &BTreeSet::from([ThemeId::builtin("builtin.spaceterm.dark")])
        ),
        Err(CatalogError::ReservedId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.themes(), before);
}

#[test]
fn catalog_never_replaces_a_reserved_builtin() {
    let mut catalog = ThemeCatalog::default();
    let before = catalog.themes();
    let reserved = terminal_theme("builtin.spaceterm.dark", "Overwrite");

    assert_eq!(
        catalog.install_batch(&[reserved], 0, &BTreeSet::new()),
        Err(CatalogError::ReservedId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.themes(), before);
}

#[test]
fn catalog_batch_failure_is_atomic_after_valid_entries() {
    let original = terminal_theme("custom.atomic-original", "Original");
    let mut catalog = ThemeCatalog::from_terminal_themes(&[original]).unwrap();
    let before = catalog.themes();
    let batch = [
        terminal_theme("custom.atomic-new", "New"),
        terminal_theme("custom.atomic-new", "Duplicate"),
    ];

    assert_eq!(
        catalog.install_batch(&batch, 0, &BTreeSet::new()),
        Err(CatalogError::DuplicateId)
    );
    assert_eq!(catalog.revision(), 0);
    assert_eq!(catalog.themes(), before);
}

#[test]
fn catalog_rejects_stale_batches_without_mutation() {
    let first = terminal_theme("custom.first", "First");
    let mut catalog = ThemeCatalog::default();
    catalog
        .install_batch(&[first], 0, &BTreeSet::new())
        .unwrap();
    let before = catalog.themes();

    assert_eq!(
        catalog.install_batch(
            &[terminal_theme("custom.stale", "Stale")],
            0,
            &BTreeSet::new()
        ),
        Err(CatalogError::RevisionConflict)
    );
    assert_eq!(catalog.revision(), 1);
    assert_eq!(catalog.themes(), before);
}

#[test]
fn reinstalling_a_zed_family_replaces_its_themes_in_place() {
    let bytes = include_bytes!("fixtures/vague-pro/theme.json");
    let imported = translate_zed_family(bytes).unwrap();
    let mut catalog = ThemeCatalog::default();
    let ids = catalog
        .install_batch(&imported, 0, &BTreeSet::new())
        .unwrap();

    let reimported = translate_zed_family(bytes).unwrap();
    assert_eq!(imported, reimported);
    assert_eq!(
        catalog
            .install_batch(&reimported, 1, &BTreeSet::new())
            .unwrap(),
        ids
    );
    assert_eq!(catalog.revision(), 2);
    assert_eq!(imported_theme_count(&catalog), ids.len());
}

#[test]
fn catalog_theme_listing_is_globally_sorted_and_includes_builtins() {
    let catalog = ThemeCatalog::from_terminal_themes(&[
        terminal_theme("custom.z-last", "Last"),
        terminal_theme("custom.a-first", "First"),
    ])
    .unwrap();
    let themes = catalog.themes();
    let ids = themes.iter().map(|theme| &theme.id).collect::<Vec<_>>();

    assert_eq!(themes.len(), 4);
    assert!(ids.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn summaries_list_builtins_first_then_installed_themes_by_name() {
    let extension = ZedExtension {
        id: String::from("sample-themes"),
        version: String::from("1.0.0"),
        families: vec![
            br#"{"name":"Sample","themes":[{"name":"alpha","appearance":"dark","style":{}}]}"#
                .to_vec(),
        ],
    };
    let mut themes = translate_zed_extension(&extension).unwrap();
    themes.push(terminal_theme("custom.zulu", "Zulu"));
    themes.push(terminal_theme("custom.beta", "Beta"));
    let catalog = ThemeCatalog::from_terminal_themes(&themes).unwrap();
    let summaries = catalog.summaries();
    let names = summaries
        .iter()
        .map(|summary| summary.name.as_str())
        .collect::<Vec<_>>();

    assert!(summaries[..2].iter().all(|summary| summary.builtin));
    assert_eq!(names[2..], ["alpha", "Beta", "Zulu"]);
    assert_eq!(summaries[2].family.as_deref(), Some("Sample"));
    assert_eq!(
        summaries[2].package,
        Some(ThemePackage {
            id: String::from("sample-themes"),
            version: String::from("1.0.0"),
        })
    );
    assert_eq!(summaries[3].family, None);
    assert_eq!(summaries[3].package, None);
}
