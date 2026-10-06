//! Translates Zed theme families into Terminal Themes, reading only terminal, cursor, selection,
//! search, and link roles. Absent roles derive from the theme, then from the built-in palette.

use std::collections::BTreeSet;

use serde::Deserialize as _;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::terminal_theme::{
    TerminalColorOverrides, ThemeMetadata, ThemeOrigin, ThemeSourceFormat, validate_text,
    validate_theme,
};
use super::{Appearance, Color, TerminalColors, TerminalTheme, ThemeId, builtin};

/// The largest Zed family document SpaceTerm reads.
pub(crate) const MAX_FAMILY_BYTES: usize = 4 * 1024 * 1024;
/// The most themes one Zed family document may contain.
pub(crate) const MAX_FAMILY_THEMES: usize = 256;
/// The most family documents one Zed extension may contribute. Every family holds at least one
/// theme, so an extension with more families than the catalog holds themes could never install.
pub(crate) const MAX_EXTENSION_FAMILIES: usize = super::terminal_theme::MAX_INSTALLED_THEMES;

const ANSI_NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// Why a Zed source installs nothing. Content-free: it names the defect, never the document.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ImportError {
    #[error("theme document is too large")]
    TooLarge,
    #[error("theme document is invalid JSON")]
    InvalidJson,
    #[error("theme source contains no usable themes or too many")]
    InvalidThemeCount,
    #[error("theme source contains an invalid theme")]
    InvalidTheme,
    #[error("Zed theme document is invalid")]
    InvalidZedDocument,
    #[error("theme cannot be serialized")]
    Serialization,
}

/// A Zed theme extension as its registry publishes it, reduced to its theme family documents.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ZedExtension {
    pub(crate) id: String,
    pub(crate) version: String,
    pub(crate) families: Vec<Vec<u8>>,
}

/// Translates every theme in one Zed family document the user selected.
///
/// A structurally invalid family installs nothing, because the user chose this one document.
pub(crate) fn translate_zed_family(bytes: &[u8]) -> Result<Vec<TerminalTheme>, ImportError> {
    let themes = translate_family(bytes, None)?;
    if themes.is_empty() {
        return Err(ImportError::InvalidThemeCount);
    }
    Ok(themes)
}

/// Translates every family an extension contributes.
///
/// Zed loads each family document independently, so one malformed family does not withhold the
/// rest of the extension. An extension without any usable theme installs nothing.
pub(crate) fn translate_zed_extension(
    extension: &ZedExtension,
) -> Result<Vec<TerminalTheme>, ImportError> {
    validate_text(&extension.id, 256).map_err(|_| ImportError::InvalidZedDocument)?;
    validate_text(&extension.version, 64).map_err(|_| ImportError::InvalidZedDocument)?;
    if extension.families.len() > MAX_EXTENSION_FAMILIES {
        return Err(ImportError::InvalidThemeCount);
    }
    let mut ids = BTreeSet::new();
    let themes = extension
        .families
        .iter()
        .filter_map(|bytes| translate_family(bytes, Some(extension)).ok())
        .flatten()
        .filter(|theme| ids.insert(theme.id.clone()))
        .collect::<Vec<_>>();
    if themes.is_empty() {
        return Err(ImportError::InvalidThemeCount);
    }
    Ok(themes)
}

fn translate_family(
    bytes: &[u8],
    extension: Option<&ZedExtension>,
) -> Result<Vec<TerminalTheme>, ImportError> {
    let root = parse_document(bytes)?;
    let family = source_text(&root, "name", 256).unwrap_or_else(|| String::from("Zed themes"));
    let author = source_text(&root, "author", 256);
    let entries = root
        .get("themes")
        .and_then(Value::as_array)
        .ok_or(ImportError::InvalidZedDocument)?;
    if entries.len() > MAX_FAMILY_THEMES {
        return Err(ImportError::InvalidThemeCount);
    }
    let mut ids = BTreeSet::new();
    let mut themes = Vec::with_capacity(entries.len());
    for entry in entries {
        let entry = entry.as_object().ok_or(ImportError::InvalidZedDocument)?;
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| validate_text(name, 128).is_ok())
            .ok_or(ImportError::InvalidZedDocument)?;
        let appearance = match entry.get("appearance").and_then(Value::as_str) {
            Some("light") => Appearance::Light,
            Some("dark") => Appearance::Dark,
            _ => return Err(ImportError::InvalidZedDocument),
        };
        let empty = Map::new();
        let style = entry
            .get("style")
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let id = theme_id(extension, &family, author.as_deref(), name, appearance)?;
        // Zed registers themes by name, so a repeated name within one source replaces nothing.
        if !ids.insert(id.clone()) {
            continue;
        }
        let canonical = serde_json::to_vec(entry).map_err(|_| ImportError::Serialization)?;
        let theme = TerminalTheme {
            id,
            name: name.to_owned(),
            appearance,
            metadata: ThemeMetadata {
                origin: Some(ThemeOrigin {
                    format: ThemeSourceFormat::Zed,
                    package_id: extension.map(|extension| extension.id.clone()),
                    package_version: extension.map(|extension| extension.version.clone()),
                    family: family.clone(),
                    theme: name.to_owned(),
                    fingerprint: format!("{:x}", Sha256::digest(canonical)),
                }),
                author: author.clone(),
                license: None,
                description: None,
            },
            colors: TerminalColorOverrides::complete(&terminal_colors(style, appearance)),
        };
        validate_theme(&theme, true).map_err(|_| ImportError::InvalidTheme)?;
        themes.push(theme);
    }
    Ok(themes)
}

/// Reads a family document the way Zed does: JSON that may contain comments and trailing commas,
/// where a repeated key takes its last value. The parser bounds nesting depth itself.
fn parse_document(bytes: &[u8]) -> Result<Value, ImportError> {
    if bytes.len() > MAX_FAMILY_BYTES {
        return Err(ImportError::TooLarge);
    }
    let mut deserializer = serde_json_lenient::Deserializer::from_slice(bytes);
    let root = Value::deserialize(&mut deserializer).map_err(|_| ImportError::InvalidJson)?;
    deserializer.end().map_err(|_| ImportError::InvalidJson)?;
    Ok(root)
}

/// A repeatable identity, so reinstalling the same source replaces its themes in place and keeps
/// every selection that names them. An extension's identity survives authorship and family
/// renames between versions; a file's identity is everything it states about itself.
fn theme_id(
    extension: Option<&ZedExtension>,
    family: &str,
    author: Option<&str>,
    name: &str,
    appearance: Appearance,
) -> Result<ThemeId, ImportError> {
    let identity = match extension {
        Some(extension) => serde_json::to_vec(&("extension", &extension.id, name, appearance)),
        None => serde_json::to_vec(&("file", family, author, name, appearance)),
    }
    .map_err(|_| ImportError::Serialization)?;
    ThemeId::new(format!("zed.{:x}", Sha256::digest(identity)))
        .map_err(|_| ImportError::InvalidTheme)
}

fn source_text(root: &Value, key: &str, max: usize) -> Option<String> {
    root.get(key)
        .and_then(Value::as_str)
        .filter(|value| validate_text(value, max).is_ok())
        .map(str::to_owned)
}

/// The first key whose value parses as a color. Zed treats an unparseable color as absent.
fn color(style: &Map<String, Value>, keys: &[&str]) -> Option<Color> {
    keys.iter().find_map(|key| {
        style
            .get(*key)?
            .as_str()
            .and_then(|value| Color::parse(value).ok())
    })
}

fn player_color(style: &Map<String, Value>, key: &str) -> Option<Color> {
    style
        .get("players")?
        .as_array()?
        .first()?
        .get(key)?
        .as_str()
        .and_then(|value| Color::parse(value).ok())
}

/// Resolves a translucent protocol color against what it would be painted over.
fn opaque(color: Color, under: Color) -> Color {
    if color.is_opaque() {
        color
    } else {
        color.source_over(under.with_alpha(0xff))
    }
}

fn terminal_colors(style: &Map<String, Value>, appearance: Appearance) -> TerminalColors {
    let base = builtin::terminal_base(appearance);
    let background = opaque(
        color(
            style,
            &[
                "terminal.background",
                "terminal.ansi.background",
                "editor.background",
                "background",
            ],
        )
        .unwrap_or(base.background),
        base.background,
    );
    let over = |color: Color| opaque(color, background);
    let foreground = over(
        color(style, &["terminal.foreground", "editor.foreground", "text"])
            .unwrap_or(base.foreground),
    );
    let palette = |prefix: &str, fallback: [Color; 8]| -> [Color; 8] {
        std::array::from_fn(|index| {
            color(style, &[&format!("{prefix}{}", ANSI_NAMES[index])]).map_or(fallback[index], over)
        })
    };
    // A theme that authors its own normal palette keeps its hues in every register, so missing
    // bright and dim colors derive from that palette rather than from SpaceTerm's.
    let normal = palette("terminal.ansi.", base.normal);
    let bright = palette("terminal.ansi.bright_", normal);
    let dim = palette(
        "terminal.ansi.dim_",
        normal.map(|color| color.mix(background, 0.35)),
    );
    let cursor = over(
        player_color(style, "cursor")
            .or_else(|| color(style, &["editor.foreground"]))
            .unwrap_or(foreground),
    );
    let find_match = color(style, &["search.match_background"]);
    TerminalColors {
        foreground,
        background,
        bright_foreground: color(style, &["terminal.bright_foreground"]).map_or(foreground, over),
        dim_foreground: color(style, &["terminal.dim_foreground"])
            .map_or_else(|| foreground.mix(background, 0.4), over),
        normal,
        bright,
        dim,
        cursor,
        cursor_text: None,
        selection_background: player_color(style, "selection")
            .unwrap_or_else(|| cursor.with_alpha(0x40)),
        selection_foreground: None,
        find_match_background: find_match.unwrap_or_else(|| normal[3].with_alpha(0x66)),
        find_match_foreground: None,
        // Older themes author one search highlight. The active match takes the same hue and closes
        // half the remaining distance to opaque, so it still reads as the current one.
        find_active_match_background: color(style, &["search.active_match_background"])
            .or_else(|| find_match.map(|color| color.with_alpha(color.a / 2 + 0x80)))
            .unwrap_or_else(|| normal[3].with_alpha(0x99)),
        find_active_match_foreground: None,
        hyperlink: over(color(style, &["link_text.hover", "text.accent"]).unwrap_or(normal[4])),
        visual_bell: normal[3].with_alpha(0x80),
    }
}
