use serde_json::json;

use super::*;

/// Third-party backgrounds that sit far from SpaceTerm's Pane band, with their authored hue.
const DARK_REFERENCES: [u32; 4] = [
    0x2e3440, // Nord: a bright slab above the root.
    0x002b36, // Solarized Dark: a saturated block.
    0x0d1117, // GitHub Dark: darker than the root.
    0x141415, // Vague: just below the root.
];
const LIGHT_REFERENCES: [u32; 3] = [
    0xfbf1c7, // Gruvbox Light: saturated cream.
    0xfdf6e3, // Solarized Light.
    0xffffff, // Pure white.
];

fn installed(appearance: &str, style: serde_json::Value) -> TerminalTheme {
    let family = serde_json::to_vec(&json!({
        "name": "Fixture",
        "themes": [{ "name": "Only", "appearance": appearance, "style": style }],
    }))
    .unwrap();
    translate_zed_family(&family).unwrap().remove(0)
}

fn preferences_for(theme: &TerminalTheme) -> AppearancePreferences {
    let mut preferences = AppearancePreferences {
        mode: match theme.appearance {
            Appearance::Light => AppearanceMode::Light,
            Appearance::Dark => AppearanceMode::Dark,
        },
        ..Default::default()
    };
    preferences
        .terminal
        .themes
        .set(theme.appearance, theme.id.clone());
    preferences
}

fn resolve(catalog: &ThemeCatalog, preferences: &AppearancePreferences) -> ResolvedAppearance {
    catalog
        .resolve(
            AppearanceGeneration::INITIAL,
            preferences,
            SystemAppearance::unavailable(),
            &AvailableFonts::default(),
        )
        .unwrap()
}

fn resolve_theme(theme: &TerminalTheme) -> ResolvedAppearance {
    let catalog = ThemeCatalog::from_terminal_themes(std::slice::from_ref(theme)).unwrap();
    resolve(&catalog, &preferences_for(theme))
}

/// Oklab lightness, chroma, and hue in degrees, computed here so the expectation does not reuse
/// the implementation's conversion.
fn oklch(color: Color) -> (f64, f64, f64) {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    let [r, g, b] = [color.r, color.g, color.b].map(linear);
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    let lightness = 0.210_454_255_3 * l + 0.793_617_785 * m - 0.004_072_046_8 * s;
    let a = 1.977_998_495_1 * l - 2.428_592_205 * m + 0.450_593_709_9 * s;
    let b = 0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766 * s;
    (lightness, a.hypot(b), b.atan2(a).to_degrees())
}

fn hue_distance(left: f64, right: f64) -> f64 {
    let distance = (left - right).rem_euclid(360.0);
    distance.min(360.0 - distance)
}

#[test]
fn builtin_terminal_themes_resolve_to_their_authored_colors() {
    for mode in [AppearanceMode::Light, AppearanceMode::Dark] {
        let resolved = resolve(
            &ThemeCatalog::default(),
            &AppearancePreferences {
                mode,
                ..Default::default()
            },
        );
        assert_eq!(
            resolved.terminal.colors,
            builtin_terminal_base(resolved.terminal.appearance),
            "{mode:?}",
        );
    }
}

#[test]
fn third_party_backgrounds_rest_one_quiet_step_above_the_chrome_root() {
    let cases = DARK_REFERENCES
        .map(|background| ("dark", background, 0.06, 0.035))
        .into_iter()
        .chain(LIGHT_REFERENCES.map(|background| ("light", background, 0.08, 0.03)));
    for (appearance, authored, widest_step, chroma_ceiling) in cases {
        let authored = Color::rgb(authored);
        let theme = installed(
            appearance,
            json!({ "terminal.background": authored.canonical_hex() }),
        );
        let resolved = resolve_theme(&theme);
        let pane = resolved.terminal.colors.background;
        let (root, _, _) = oklch(resolved.chrome.colors.background);
        let (lightness, chroma, hue) = oklch(pane);
        let (_, authored_chroma, authored_hue) = oklch(authored);

        assert!(
            lightness > root && lightness - root <= widest_step,
            "{authored:?} rests one quiet step above the root: pane={pane:?}",
        );
        assert!(
            chroma <= chroma_ceiling + 0.002,
            "{authored:?} keeps a quiet chroma: pane={pane:?} chroma={chroma}",
        );
        if authored_chroma > 0.01 {
            assert!(
                hue_distance(hue, authored_hue) <= 10.0,
                "{authored:?} keeps its hue: pane={pane:?} hue={hue} authored={authored_hue}",
            );
        }
    }
}

fn text_colors(colors: &TerminalColors) -> Vec<(&'static str, Color)> {
    let mut text = vec![
        ("foreground", colors.foreground),
        ("bright_foreground", colors.bright_foreground),
        ("dim_foreground", colors.dim_foreground),
        ("cursor", colors.cursor),
        ("hyperlink", colors.hyperlink),
    ];
    for (register, palette) in [
        ("normal", colors.normal),
        ("bright", colors.bright),
        ("dim", colors.dim),
    ] {
        text.extend(palette.map(|color| (register, color)));
    }
    text
}

#[test]
fn harmonized_backgrounds_keep_the_contrast_their_text_colors_were_authored_with() {
    // GitHub Dark lifts above the root; its gray ANSI black and comment-like dim foreground lose
    // contrast against the lighter background unless restored.
    let theme = installed(
        "dark",
        json!({
            "terminal.background": "#0d1117",
            "terminal.foreground": "#e6edf3",
            "terminal.dim_foreground": "#6e7681",
            "terminal.ansi.black": "#484f58",
            "terminal.ansi.bright_black": "#6e7681",
            "terminal.ansi.blue": "#2f81f7",
        }),
    );
    let mut authored = builtin_terminal_base(Appearance::Dark);
    authored.apply(&theme.colors);
    let resolved = resolve_theme(&theme);
    let colors = &resolved.terminal.colors;
    assert_ne!(colors.background, authored.background);

    for ((role, before), (_, after)) in text_colors(&authored).into_iter().zip(text_colors(colors))
    {
        let required = before.contrast_ratio(authored.background).min(4.5);
        let achieved = after.contrast_ratio(colors.background);
        assert!(
            achieved >= required - 0.01,
            "{role} {before:?} keeps contrast {required}: now {after:?} at {achieved}",
        );
    }
}

#[test]
fn a_background_override_is_used_as_authored() {
    let theme = installed(
        "dark",
        json!({
            "terminal.background": "#0d1117",
            "terminal.dim_foreground": "#6e7681",
        }),
    );
    let catalog = ThemeCatalog::from_terminal_themes(std::slice::from_ref(&theme)).unwrap();
    let mut preferences = preferences_for(&theme);
    preferences.terminal.overrides.insert(
        theme.id.clone(),
        TerminalColorOverrides {
            background: Some(Color::rgb(0x0d1117)),
            ..Default::default()
        },
    );
    let colors = resolve(&catalog, &preferences).terminal.colors.clone();

    assert_eq!(colors.background, Color::rgb(0x0d1117));
    assert_eq!(colors.dim_foreground, Color::rgb(0x6e7681));
}

#[test]
fn gallery_previews_show_the_palette_the_pane_presents() {
    let theme = installed("dark", json!({ "terminal.background": "#2e3440" }));
    let catalog = ThemeCatalog::from_terminal_themes(std::slice::from_ref(&theme)).unwrap();
    let summary = catalog
        .summaries()
        .into_iter()
        .find(|summary| summary.id == theme.id)
        .unwrap();

    assert_eq!(
        summary.colors,
        resolve(&catalog, &preferences_for(&theme)).terminal.colors
    );
}
