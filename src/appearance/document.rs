use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::preferences::ResetTarget;

use super::{Appearance, AppearancePreferences, SchemeCatalog, SchemeSlots};
use super::{
    Color, SchemeId,
    scheme::{CatalogError, ColorScheme, SchemeMetadata, TerminalColorOverrides, validate_scheme},
};

const SETTINGS_SCHEMA_VERSION: u32 = 2;
const COLOR_SCHEME_SCHEMA_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_IMPORT_SCHEMES: usize = 32;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsDocument {
    pub(crate) schema_version: u32,
    pub(crate) revision: u64,
    pub(crate) preferences: AppearancePreferences,
    #[serde(default)]
    pub(crate) color_schemes: Vec<ColorScheme>,
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            revision: 0,
            preferences: AppearancePreferences::default(),
            color_schemes: Vec::new(),
        }
    }
}

impl SettingsDocument {
    pub(crate) fn validate(&self) -> Result<(), SettingsDocumentError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsDocumentError::UnsupportedVersion);
        }
        self.preferences
            .validate()
            .map_err(|_| SettingsDocumentError::InvalidPreferences)?;
        let catalog = SchemeCatalog::from_color_schemes(&self.color_schemes)
            .map_err(|_| SettingsDocumentError::InvalidCatalog)?;
        validate_selection(&catalog, &self.preferences.terminal.schemes)?;
        Ok(())
    }

    pub(crate) fn reset(&mut self, target: ResetTarget) -> Result<(), SettingsDocumentError> {
        let mut candidate = self.clone();
        candidate.preferences.reset(target);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Returns every Setting this document owns to its default, imported schemes included.
    ///
    /// Preferences and the imported catalog reset together because they constrain each other: a
    /// selection naming an imported scheme is only valid while that scheme is installed. Clearing
    /// the catalog alone would strand such a selection, and defaulting preferences alone would
    /// leave a library the reset claims to have emptied. The identity fields carry the document
    /// forward instead: `revision` orders the write against concurrent editors, and
    /// `schema_version` states the format this build writes.
    pub(crate) fn reset_all(&mut self) {
        let defaults = Self::default();
        self.preferences = defaults.preferences;
        self.color_schemes = defaults.color_schemes;
    }
}

fn validate_selection(
    catalog: &SchemeCatalog,
    slots: &SchemeSlots,
) -> Result<(), SettingsDocumentError> {
    let selections = [
        (&slots.light, Appearance::Light),
        (&slots.dark, Appearance::Dark),
    ];
    for (id, expected) in selections {
        let same = catalog.get(id).map(|scheme| scheme.appearance);
        if same.is_some_and(|actual| actual != expected) {
            return Err(SettingsDocumentError::InvalidPreferences);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ColorSchemeDocument {
    pub(crate) schema_version: u32,
    pub(crate) schemes: Vec<ColorScheme>,
}

pub(crate) fn parse_settings(bytes: &[u8]) -> Result<SettingsDocument, SettingsDocumentError> {
    preflight(bytes).map_err(SettingsDocumentError::from_preflight)?;
    let document: SettingsDocument =
        serde_json::from_slice(bytes).map_err(|_| SettingsDocumentError::InvalidJson)?;
    document.validate()?;
    Ok(document)
}

pub(crate) fn export_settings(
    document: &SettingsDocument,
) -> Result<String, SettingsDocumentError> {
    document.validate()?;
    serde_json::to_string_pretty(document)
        .map(|mut output| {
            output.push('\n');
            output
        })
        .map_err(|_| SettingsDocumentError::Serialization)
}

pub(crate) fn parse_color_document(bytes: &[u8]) -> Result<ColorSchemeDocument, ImportError> {
    preflight(bytes).map_err(ImportError::from_preflight)?;
    let document: ColorSchemeDocument =
        serde_json::from_slice(bytes).map_err(|_| ImportError::InvalidJson)?;
    validate_color_document(&document)?;
    Ok(document)
}

pub(crate) fn export_color_document(document: &ColorSchemeDocument) -> Result<String, ImportError> {
    validate_color_document(document)?;
    serde_json::to_string_pretty(document)
        .map(|mut output| {
            output.push('\n');
            output
        })
        .map_err(|_| ImportError::Serialization)
}

/// Capture precisely the published colors, including missing-request fallback behavior.
pub(crate) fn export_resolved_schemes(
    catalog: &SchemeCatalog,
    resolved: &super::ResolvedAppearance,
) -> Result<String, ImportError> {
    let mut terminal = catalog
        .get(&resolved.terminal.effective_scheme)
        .ok_or(ImportError::UnknownScheme)?
        .clone();
    terminal.id = portable_id(&terminal.id, true)?;
    terminal.colors = TerminalColorOverrides::complete(&resolved.terminal.colors);
    export_color_document(&ColorSchemeDocument {
        schema_version: COLOR_SCHEME_SCHEMA_VERSION,
        schemes: vec![terminal],
    })
}

fn portable_id(id: &SchemeId, effective: bool) -> Result<SchemeId, ImportError> {
    if !effective && !id.is_reserved() {
        return Ok(id.clone());
    }
    let hash = format!("{:x}", Sha256::digest(id.as_str().as_bytes()));
    SchemeId::new(format!("copy.{hash}")).map_err(|_| ImportError::InvalidScheme)
}

/// Export authored definitions, giving built-ins complete palettes and portable identities.
pub(crate) fn export_schemes(
    catalog: &SchemeCatalog,
    schemes: &[SchemeId],
) -> Result<String, ImportError> {
    if schemes.is_empty() || schemes.len() > MAX_IMPORT_SCHEMES {
        return Err(ImportError::InvalidSchemeCount);
    }
    let mut ids = BTreeSet::new();
    let mut exported = Vec::with_capacity(schemes.len());
    for id in schemes {
        if !ids.insert(id.clone()) {
            return Err(ImportError::DuplicateId);
        }
        let mut scheme = catalog
            .get(id)
            .ok_or(ImportError::UnknownScheme)?
            .clone();
        // Terminal definitions retain their documented palette fallback. Flatten built-in
        // copies so installation preserves every protocol color without reserved identity.
        if id.is_reserved() {
            let mut colors = super::builtin::terminal_base(scheme.appearance);
            colors.apply(&scheme.colors);

            scheme.colors = TerminalColorOverrides::complete(&colors);
        }
        scheme.id = portable_id(id, false)?;
        exported.push(scheme);
    }

    export_color_document(&ColorSchemeDocument {
        schema_version: COLOR_SCHEME_SCHEMA_VERSION,
        schemes: exported,
    })
}

fn validate_color_document(document: &ColorSchemeDocument) -> Result<(), ImportError> {
    if document.schema_version != COLOR_SCHEME_SCHEMA_VERSION {
        return Err(ImportError::UnsupportedVersion);
    }
    if document.schemes.is_empty() || document.schemes.len() > MAX_IMPORT_SCHEMES {
        return Err(ImportError::InvalidSchemeCount);
    }
    let mut ids = BTreeSet::new();
    for scheme in &document.schemes {
        validate_scheme(scheme, true).map_err(|_| ImportError::InvalidScheme)?;
        if !ids.insert(scheme.id.clone()) {
            return Err(ImportError::DuplicateId);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImportCandidate {
    pub(crate) index: usize,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
}

pub(crate) fn list_zed_candidates(bytes: &[u8]) -> Result<Vec<ImportCandidate>, ImportError> {
    let root = parse_zed(bytes)?;
    zed_themes(&root)?
        .iter()
        .enumerate()
        .map(|(index, theme)| {
            let object = theme.as_object().ok_or(ImportError::InvalidZedDocument)?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or(ImportError::InvalidZedDocument)?;
            validate_zed_name(name)?;
            Ok(ImportCandidate {
                index,
                name: name.to_owned(),
                appearance: zed_appearance(object)?,
            })
        })
        .collect()
}

pub(crate) fn import_zed(bytes: &[u8], candidate_index: usize) -> Result<ColorScheme, ImportError> {
    let root = parse_zed(bytes)?;
    let themes = zed_themes(&root)?;
    let theme = themes
        .get(candidate_index)
        .ok_or(ImportError::UnknownCandidate)?
        .as_object()
        .ok_or(ImportError::InvalidZedDocument)?;
    let name = theme
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ImportError::InvalidZedDocument)?;
    validate_zed_name(name)?;
    let appearance = zed_appearance(theme)?;
    let style = theme
        .get("style")
        .and_then(Value::as_object)
        .ok_or(ImportError::InvalidZedDocument)?;
    let author = source_text(&root, "author", 256)?;
    let family =
        source_text(&root, "name", 256)?.unwrap_or_else(|| String::from("Unidentified Zed family"));
    // These descriptors establish a repeatable import namespace, not source ownership. A collision
    // always requires explicit catalog replacement; no name or origin can authorize that operation.
    let source_identity = serde_json::to_vec(&(
        source_text(&root, "id", 256)?,
        &family,
        &author,
        name,
        appearance,
    ))
    .map_err(|_| ImportError::Serialization)?;
    let hash = format!("{:x}", Sha256::digest(source_identity));
    let canonical_candidate = serde_json::to_vec(theme).map_err(|_| ImportError::Serialization)?;
    let metadata = SchemeMetadata {
        origin: Some(super::scheme::SchemeOrigin {
            format: super::scheme::SchemeSourceFormat::Zed,
            package_id: source_text(&root, "id", 256)?,
            family,
            theme: name.to_owned(),
            fingerprint: format!("{:x}", Sha256::digest(canonical_candidate)),
        }),
        author,
        license: source_text(&root, "license", 256)?,
        description: Some(String::from("Color roles translated from Zed by SpaceTerm")),
    };
    let scheme = ColorScheme {
        id: SchemeId::new(format!("import.{hash}")).map_err(|_| ImportError::InvalidScheme)?,
        name: name.to_owned(),
        appearance,
        metadata,
        colors: zed_terminal(style)?,
    };
    validate_scheme(&scheme, true).map_err(|_| ImportError::InvalidScheme)?;
    Ok(scheme)
}

fn source_text(root: &Value, key: &str, max: usize) -> Result<Option<String>, ImportError> {
    match root.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            super::scheme::validate_text(value, max).map_err(|_| ImportError::InvalidScheme)?;
            Ok(Some(value.clone()))
        }
        Some(_) => Err(ImportError::InvalidZedDocument),
    }
}

fn parse_zed(bytes: &[u8]) -> Result<Value, ImportError> {
    preflight(bytes).map_err(ImportError::from_preflight)?;
    serde_json::from_slice(bytes).map_err(|_| ImportError::InvalidZedDocument)
}

fn zed_themes(root: &Value) -> Result<&Vec<Value>, ImportError> {
    let themes = root
        .get("themes")
        .and_then(Value::as_array)
        .ok_or(ImportError::InvalidZedDocument)?;
    if themes.is_empty() || themes.len() > MAX_IMPORT_SCHEMES {
        return Err(ImportError::InvalidSchemeCount);
    }
    let mut identities = BTreeSet::new();
    for theme in themes {
        let object = theme.as_object().ok_or(ImportError::InvalidZedDocument)?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ImportError::InvalidZedDocument)?;
        validate_zed_name(name)?;
        zed_appearance(object)?;
        if !identities.insert(name) {
            return Err(ImportError::AmbiguousCandidate);
        }
    }
    Ok(themes)
}

fn zed_appearance(object: &serde_json::Map<String, Value>) -> Result<Appearance, ImportError> {
    match object.get("appearance").and_then(Value::as_str) {
        Some("light") => Ok(Appearance::Light),
        Some("dark") => Ok(Appearance::Dark),
        _ => Err(ImportError::InvalidZedDocument),
    }
}

fn validate_zed_name(name: &str) -> Result<(), ImportError> {
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        Err(ImportError::InvalidScheme)
    } else {
        Ok(())
    }
}

fn zed_color(
    style: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<Color>, ImportError> {
    match style.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Color::parse(value)
            .map(Some)
            .map_err(|()| ImportError::InvalidColor),
        Some(_) => Err(ImportError::InvalidColor),
    }
}

fn zed_terminal(
    style: &serde_json::Map<String, Value>,
) -> Result<TerminalColorOverrides, ImportError> {
    let mut colors = TerminalColorOverrides::default();
    macro_rules! map { ($($field:ident => $key:literal),+ $(,)?) => { $(colors.$field = zed_color(style, $key)?;)+ }; }
    map! { foreground => "terminal.foreground", background => "terminal.background",
    bright_foreground => "terminal.bright_foreground", dim_foreground => "terminal.dim_foreground",
    find_match_background => "search.match_background",
    find_active_match_background => "search.active_match_background", hyperlink => "link_text.hover" }
    colors.normal = zed_palette(
        style,
        "terminal.ansi.",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
    )?;
    colors.bright = zed_palette(
        style,
        "terminal.ansi.bright_",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
    )?;
    colors.dim = zed_palette(
        style,
        "terminal.ansi.dim_",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
    )?;
    if let Some(foreground) = colors.foreground {
        colors.cursor = Some(foreground);
    }
    if let Some(players) = style.get("players").and_then(Value::as_array)
        && let Some(selection) = players
            .first()
            .and_then(Value::as_object)
            .and_then(|player| player.get("selection"))
    {
        colors.selection_background = match selection {
            Value::String(value) => {
                Some(Color::parse(value).map_err(|()| ImportError::InvalidColor)?)
            }
            Value::Null => None,
            _ => return Err(ImportError::InvalidColor),
        };
    }
    colors.validate().map_err(|error| match error {
        CatalogError::UnsupportedAlpha => ImportError::UnsupportedAlpha,
        _ => ImportError::InvalidScheme,
    })?;
    Ok(colors)
}

fn zed_palette(
    style: &serde_json::Map<String, Value>,
    prefix: &str,
    names: [&str; 8],
) -> Result<Option<super::scheme::TerminalPaletteOverrides>, ImportError> {
    let mut authored = [None; 8];
    for (index, name) in names.into_iter().enumerate() {
        authored[index] = zed_color(style, &format!("{prefix}{name}"))?;
    }
    Ok(super::scheme::TerminalPaletteOverrides::sparse(authored))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PreflightError {
    TooLarge,
    InvalidJson,
    DuplicateKey,
    TooDeep,
}

fn preflight(bytes: &[u8]) -> Result<(), PreflightError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(PreflightError::TooLarge);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DepthSeed(0)
        .deserialize(&mut deserializer)
        .map_err(|error| {
            if error.to_string().contains("duplicate key") {
                PreflightError::DuplicateKey
            } else if error.to_string().contains("nesting depth") {
                PreflightError::TooDeep
            } else {
                PreflightError::InvalidJson
            }
        })?;
    deserializer.end().map_err(|_| PreflightError::InvalidJson)
}

struct DepthSeed(usize);
impl<'de> DeserializeSeed<'de> for DepthSeed {
    type Value = ();
    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.0 > MAX_DEPTH {
            return Err(de::Error::custom("nesting depth exceeded"));
        }
        deserializer.deserialize_any(DepthVisitor(self.0))
    }
}
struct DepthVisitor(usize);
impl<'de> Visitor<'de> for DepthVisitor {
    type Value = ();
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_string<E>(self, _: String) -> Result<(), E> {
        Ok(())
    }
    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }
    fn visit_some<D>(self, d: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DepthSeed(self.0 + 1).deserialize(d)
    }
    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element_seed(DepthSeed(self.0 + 1))?.is_some() {}
        Ok(())
    }
    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate key"));
            }
            map.next_value_seed(DepthSeed(self.0 + 1))?;
        }
        Ok(())
    }
    fn visit_newtype_struct<D>(self, d: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        DepthSeed(self.0 + 1).deserialize(d)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SettingsDocumentError {
    #[error("settings document is too large")]
    TooLarge,
    #[error("settings document is invalid JSON")]
    InvalidJson,
    #[error("settings document contains a duplicate key")]
    DuplicateKey,
    #[error("settings document nesting is too deep")]
    TooDeep,
    #[error("settings document version is unsupported")]
    UnsupportedVersion,
    #[error("appearance preferences are invalid")]
    InvalidPreferences,
    #[error("appearance catalog is invalid")]
    InvalidCatalog,
    #[error("settings document cannot be serialized")]
    Serialization,
}
impl SettingsDocumentError {
    fn from_preflight(error: PreflightError) -> Self {
        match error {
            PreflightError::TooLarge => Self::TooLarge,
            PreflightError::InvalidJson => Self::InvalidJson,
            PreflightError::DuplicateKey => Self::DuplicateKey,
            PreflightError::TooDeep => Self::TooDeep,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ImportError {
    #[error("import document is too large")]
    TooLarge,
    #[error("import document is invalid JSON")]
    InvalidJson,
    #[error("import document contains a duplicate key")]
    DuplicateKey,
    #[error("import document nesting is too deep")]
    TooDeep,
    #[error("import version is unsupported")]
    UnsupportedVersion,
    #[error("import contains an invalid number of schemes")]
    InvalidSchemeCount,
    #[error("import contains a duplicate scheme identity")]
    DuplicateId,
    #[error("import contains an invalid scheme")]
    InvalidScheme,
    #[error("Zed document is invalid")]
    InvalidZedDocument,
    #[error("Zed color is invalid")]
    InvalidColor,
    #[error("Zed protocol color has unsupported alpha")]
    UnsupportedAlpha,
    #[error("import candidate does not exist")]
    UnknownCandidate,
    #[error("Zed candidate identity is ambiguous")]
    AmbiguousCandidate,
    #[error("scheme does not exist")]
    UnknownScheme,
    #[error("import cannot be serialized")]
    Serialization,
}
impl ImportError {
    fn from_preflight(error: PreflightError) -> Self {
        match error {
            PreflightError::TooLarge => Self::TooLarge,
            PreflightError::InvalidJson => Self::InvalidJson,
            PreflightError::DuplicateKey => Self::DuplicateKey,
            PreflightError::TooDeep => Self::TooDeep,
        }
    }
}
