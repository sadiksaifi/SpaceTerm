use serde_json::json;

use super::terminal_theme::ThemeSourceFormat;
use super::*;

const VAGUE_PRO: &[u8] = include_bytes!("fixtures/vague-pro/theme.json");

fn family(themes: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": "Fixture",
        "author": "Fixture Author",
        "themes": themes,
    }))
    .unwrap()
}

fn one_theme(appearance: &str, style: serde_json::Value) -> Vec<u8> {
    family(json!([{ "name": "Only", "appearance": appearance, "style": style }]))
}

fn resolved(theme: &TerminalTheme) -> TerminalColors {
    let mut colors = builtin::terminal_base(theme.appearance);
    colors.apply(&theme.colors);
    colors
}

fn extension(id: &str, version: &str, families: Vec<Vec<u8>>) -> ZedExtension {
    ZedExtension {
        id: id.to_owned(),
        version: version.to_owned(),
        families,
    }
}

#[test]
fn a_published_family_translates_its_terminal_roles() {
    let themes = translate_zed_family(VAGUE_PRO).unwrap();
    let theme = &themes[0];
    let colors = resolved(theme);

    assert_eq!(theme.name, "Vague Pro");
    assert_eq!(theme.appearance, Appearance::Dark);
    assert!(theme.id.as_str().starts_with("zed."));
    assert!(!theme.id.is_reserved());
    let origin = theme.metadata.origin.as_ref().unwrap();
    assert_eq!(origin.format, ThemeSourceFormat::Zed);
    assert_eq!(origin.package_id, None);
    assert_eq!(origin.package_version, None);
    assert_eq!(origin.theme, "Vague Pro");
    assert_eq!(colors.background, Color::rgb(0x141415));
    assert_eq!(colors.foreground, Color::rgb(0xcdcdcd));
    assert_eq!(colors.normal[1], Color::rgb(0xd8647e));
    assert_eq!(colors.bright[3], Color::rgb(0xf5cb96));
    assert_eq!(colors.dim[5], Color::rgb(0x7b677c));
    assert_eq!(colors.bright_foreground, Color::rgb(0xd7d7d7));
    assert_eq!(colors.dim_foreground, Color::rgb(0x878787));
    assert_eq!(colors.cursor, Color::rgb(0xcdcdcd));
    assert_eq!(colors.selection_background, Color::rgba(0x333738aa));
    assert_eq!(colors.find_match_background, Color::rgba(0x6e94b266));
    assert_eq!(colors.find_active_match_background, Color::rgba(0xe8b58966));
    assert_eq!(colors.hyperlink, Color::rgb(0x7e98e8));
    colors.validate().unwrap();
}

#[test]
fn every_translated_theme_stores_a_complete_palette() {
    let theme = &translate_zed_family(&one_theme("light", json!({}))).unwrap()[0];
    let colors = &theme.colors;

    assert!(colors.foreground.is_some());
    assert!(colors.background.is_some());
    for palette in [&colors.normal, &colors.bright, &colors.dim] {
        let palette = palette.as_ref().unwrap();
        assert!((0..8).all(|index| palette.get(index).is_some()));
    }
    assert_eq!(resolved(theme).background, builtin::terminal_base(Appearance::Light).background);
}

#[test]
fn file_identity_is_deterministic_and_names_the_family_author_theme_and_appearance() {
    let first = translate_zed_family(VAGUE_PRO).unwrap();
    let second = translate_zed_family(VAGUE_PRO).unwrap();
    assert_eq!(first, second);

    let dark = &translate_zed_family(&one_theme("dark", json!({}))).unwrap()[0];
    let light = &translate_zed_family(&one_theme("light", json!({}))).unwrap()[0];
    let renamed = serde_json::to_vec(&json!({
        "name": "Renamed",
        "author": "Fixture Author",
        "themes": [{ "name": "Only", "appearance": "dark", "style": {} }],
    }))
    .unwrap();
    let renamed = &translate_zed_family(&renamed).unwrap()[0];

    assert_ne!(dark.id, light.id);
    assert_ne!(dark.id, renamed.id);
}

#[test]
fn extension_identity_survives_family_and_author_changes_between_versions() {
    let first = translate_zed_extension(&extension(
        "vague",
        "1.0.0",
        vec![one_theme("dark", json!({ "terminal.background": "#101010" }))],
    ))
    .unwrap();
    let renamed = serde_json::to_vec(&json!({
        "name": "Vague 2",
        "author": "Someone Else",
        "themes": [{ "name": "Only", "appearance": "dark", "style": {} }],
    }))
    .unwrap();
    let second = translate_zed_extension(&extension("vague", "2.0.0", vec![renamed])).unwrap();
    let file = translate_zed_family(&one_theme("dark", json!({}))).unwrap();

    assert_eq!(first[0].id, second[0].id);
    assert_ne!(first[0].id, file[0].id);
    let origin = second[0].metadata.origin.as_ref().unwrap();
    assert_eq!(origin.package_id.as_deref(), Some("vague"));
    assert_eq!(origin.package_version.as_deref(), Some("2.0.0"));
    assert_eq!(origin.family, "Vague 2");
}

#[test]
fn an_unparseable_color_is_absent_and_the_next_source_applies() {
    let theme = &translate_zed_family(&one_theme(
        "dark",
        json!({
            "terminal.background": "not a color",
            "editor.background": "#202020",
            "terminal.foreground": 7,
            "text": "#e0e0e0",
            "terminal.ansi.red": "#zzzzzz",
        }),
    ))
    .unwrap()[0];
    let colors = resolved(theme);

    assert_eq!(colors.background, Color::rgb(0x202020));
    assert_eq!(colors.foreground, Color::rgb(0xe0e0e0));
    assert_eq!(colors.normal[1], builtin::terminal_base(Appearance::Dark).normal[1]);
}

#[test]
fn missing_registers_derive_from_the_theme_palette() {
    let theme = &translate_zed_family(&one_theme(
        "dark",
        json!({
            "terminal.background": "#000000",
            "terminal.foreground": "#ffffff",
            "terminal.ansi.red": "#ff0000",
            "players": [{ "cursor": "#00ff00" }],
            "search.match_background": "#0000ff40",
            "text.accent": "#123456",
        }),
    ))
    .unwrap()[0];
    let colors = resolved(theme);
    let red = Color::rgb(0xff0000);

    assert_eq!(colors.bright[1], red);
    assert_eq!(colors.dim[1], red.mix(Color::rgb(0x000000), 0.35));
    assert_eq!(colors.bright_foreground, Color::rgb(0xffffff));
    assert_eq!(colors.dim_foreground, Color::rgb(0xffffff).mix(Color::rgb(0x000000), 0.4));
    assert_eq!(colors.cursor, Color::rgb(0x00ff00));
    assert_eq!(colors.selection_background, Color::rgba(0x00ff0040));
    assert_eq!(colors.find_match_background, Color::rgba(0x0000ff40));
    assert_eq!(colors.find_active_match_background, Color::rgba(0x0000ffa0));
    assert_eq!(colors.hyperlink, Color::rgb(0x123456));
    assert_eq!(colors.visual_bell, colors.normal[3].with_alpha(0x80));
}

#[test]
fn translucent_protocol_colors_composite_over_the_background() {
    let theme = &translate_zed_family(&one_theme(
        "dark",
        json!({
            "terminal.background": "#ffffff80",
            "editor.background": "#00000000",
            "terminal.foreground": "#ff000080",
            "terminal.ansi.blue": "#0000ff80",
        }),
    ))
    .unwrap()[0];
    let colors = resolved(theme);
    let background = Color::rgba(0xffffff80).source_over(builtin::terminal_base(Appearance::Dark).background);

    assert_eq!(colors.background, background);
    assert_eq!(colors.foreground, Color::rgba(0xff000080).source_over(background));
    assert_eq!(colors.normal[4], Color::rgba(0x0000ff80).source_over(background));
    colors.validate().unwrap();
}

#[test]
fn a_repeated_theme_within_one_family_keeps_the_first() {
    let themes = translate_zed_family(&family(json!([
        { "name": "Same", "appearance": "dark", "style": { "terminal.background": "#111111" } },
        { "name": "Same", "appearance": "dark", "style": { "terminal.background": "#222222" } },
        { "name": "Same", "appearance": "light", "style": {} },
    ])))
    .unwrap();

    assert_eq!(themes.len(), 2);
    assert_eq!(resolved(&themes[0]).background, Color::rgb(0x111111));
}

#[test]
fn a_family_the_user_selected_installs_nothing_when_any_theme_is_malformed() {
    for themes in [
        json!([{ "name": "", "appearance": "dark" }]),
        json!([{ "name": "Unknown", "appearance": "sepia" }]),
        json!([{ "name": "Control\u{7}", "appearance": "dark" }]),
        json!(["not an object"]),
    ] {
        assert_eq!(
            translate_zed_family(&family(themes)),
            Err(ImportError::InvalidZedDocument)
        );
    }
    assert_eq!(
        translate_zed_family(br#"{"name":"No themes"}"#),
        Err(ImportError::InvalidZedDocument)
    );
    assert_eq!(
        translate_zed_family(&family(json!([]))),
        Err(ImportError::InvalidThemeCount)
    );
    assert_eq!(
        translate_zed_family(b"{"),
        Err(ImportError::InvalidJson)
    );
}

#[test]
fn an_extension_skips_a_malformed_family_and_keeps_the_rest() {
    let themes = translate_zed_extension(&extension(
        "mixed",
        "1.0.0",
        vec![
            b"{".to_vec(),
            family(json!([{ "name": "Bad", "appearance": "sepia" }])),
            one_theme("dark", json!({})),
            one_theme("dark", json!({})),
        ],
    ))
    .unwrap();

    assert_eq!(themes.len(), 1);
    assert_eq!(
        translate_zed_extension(&extension("empty", "1.0.0", vec![b"{".to_vec()])),
        Err(ImportError::InvalidThemeCount)
    );
    assert_eq!(
        translate_zed_extension(&extension("empty", "1.0.0", Vec::new())),
        Err(ImportError::InvalidThemeCount)
    );
    assert_eq!(
        translate_zed_extension(&extension(
            "many",
            "1.0.0",
            vec![one_theme("dark", json!({})); MAX_EXTENSION_FAMILIES + 1]
        )),
        Err(ImportError::InvalidThemeCount)
    );
}

#[test]
fn a_selected_zed_theme_changes_terminal_colors_and_never_chrome() {
    let theme = translate_zed_family(VAGUE_PRO).unwrap().remove(0);
    let catalog = ThemeCatalog::from_terminal_themes(std::slice::from_ref(&theme)).unwrap();
    let mut preferences = AppearancePreferences {
        mode: AppearanceMode::Dark,
        ..Default::default()
    };
    let resolve = |preferences: &AppearancePreferences| {
        catalog
            .resolve(
                AppearanceGeneration::INITIAL,
                preferences,
                SystemAppearance::unavailable(),
                &AvailableFonts::default(),
            )
            .unwrap()
    };
    let builtin = resolve(&preferences);
    preferences.terminal.themes.set(Appearance::Dark, theme.id.clone());
    let zed = resolve(&preferences);

    assert_eq!(zed.terminal.effective_theme, theme.id);
    assert_ne!(zed.terminal.colors, builtin.terminal.colors);
    assert_eq!(zed.chrome, builtin.chrome);
}

#[test]
fn family_documents_are_read_as_leniently_as_zed_reads_them() {
    let themes = translate_zed_family(
        br##"// Copyright notice
{
    "name": "Commented", /* inline */
    "themes": [
        {
            "name": "Only",
            "appearance": "dark",
            "style": {
                // Base colors
                "terminal.background": "#111111",
                "terminal.background": "#222222",
            },
        },
    ],
}
"##,
    )
    .unwrap();

    assert_eq!(resolved(&themes[0]).background, Color::rgb(0x222222));
    assert_eq!(
        translate_zed_family(&vec![b' '; MAX_FAMILY_BYTES + 1]),
        Err(ImportError::TooLarge)
    );
}
