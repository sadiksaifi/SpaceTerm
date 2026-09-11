use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::preferences::ResetTarget;
use super::scheme::CustomScheme;
use super::{Appearance, AppearancePreferences, SchemeCatalog, SchemeKind, SchemeSelection};
use super::{
    Color, SchemeId,
    scheme::{
        CatalogError, ChromeColorOverrides, ChromeScheme, SchemeMetadata, TerminalColorOverrides,
        TerminalScheme, validate_scheme,
    },
};

const SCHEMA_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;
const MAX_IMPORT_SCHEMES: usize = 32;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppearanceDocument {
    pub(crate) schema_version: u32,
    pub(crate) revision: u64,
    pub(crate) preferences: AppearancePreferences,
    #[serde(default)]
    pub(crate) custom_schemes: Vec<CustomScheme>,
}

impl Default for AppearanceDocument {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            revision: 0,
            preferences: AppearancePreferences::default(),
            custom_schemes: Vec::new(),
        }
    }
}

impl AppearanceDocument {
    pub(crate) fn validate(&self) -> Result<(), AppearanceDocumentError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(AppearanceDocumentError::UnsupportedVersion);
        }
        self.preferences
            .validate()
            .map_err(|_| AppearanceDocumentError::InvalidPreferences)?;
        let catalog = SchemeCatalog::from_custom_schemes(&self.custom_schemes)
            .map_err(|_| AppearanceDocumentError::InvalidCatalog)?;
        if self
            .preferences
            .chrome
            .overrides
            .keys()
            .any(|id| catalog.terminal(id).is_some())
            || self
                .preferences
                .terminal
                .overrides
                .keys()
                .any(|id| catalog.chrome(id).is_some())
        {
            return Err(AppearanceDocumentError::InvalidPreferences);
        }
        validate_selection(
            &catalog,
            &self.preferences.chrome.scheme,
            SchemeKind::Chrome,
        )?;
        validate_selection(
            &catalog,
            &self.preferences.terminal.scheme,
            SchemeKind::Terminal,
        )?;
        Ok(())
    }

    pub(crate) fn reset(&mut self, target: ResetTarget) -> Result<(), AppearanceDocumentError> {
        let mut candidate = self.clone();
        candidate.preferences.reset(target);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
}

fn validate_selection(
    catalog: &SchemeCatalog,
    selection: &SchemeSelection,
    kind: SchemeKind,
) -> Result<(), AppearanceDocumentError> {
    let selections = match selection {
        SchemeSelection::Fixed { id, appearance } => vec![(id, *appearance)],
        SchemeSelection::System { light, dark } => {
            vec![(light, Appearance::Light), (dark, Appearance::Dark)]
        }
    };
    for (id, expected) in selections {
        let (same, other) = match kind {
            SchemeKind::Chrome => (
                catalog.chrome(id).map(|scheme| scheme.appearance),
                catalog.terminal(id).is_some(),
            ),
            SchemeKind::Terminal => (
                catalog.terminal(id).map(|scheme| scheme.appearance),
                catalog.chrome(id).is_some(),
            ),
        };
        if other || same.is_some_and(|actual| actual != expected) {
            return Err(AppearanceDocumentError::InvalidPreferences);
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ColorSchemeDocument {
    pub(crate) schema_version: u32,
    pub(crate) schemes: Vec<CustomScheme>,
}

pub(crate) fn parse_settings(bytes: &[u8]) -> Result<AppearanceDocument, AppearanceDocumentError> {
    preflight(bytes).map_err(AppearanceDocumentError::from_preflight)?;
    let document: AppearanceDocument =
        serde_json::from_slice(bytes).map_err(|_| AppearanceDocumentError::InvalidJson)?;
    document.validate()?;
    Ok(document)
}

pub(crate) fn export_settings(
    document: &AppearanceDocument,
) -> Result<String, AppearanceDocumentError> {
    document.validate()?;
    serde_json::to_string_pretty(document)
        .map(|mut output| {
            output.push('\n');
            output
        })
        .map_err(|_| AppearanceDocumentError::Serialization)
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

pub(crate) fn export_schemes(
    catalog: &SchemeCatalog,
    schemes: &[(SchemeKind, SchemeId)],
) -> Result<String, ImportError> {
    if schemes.is_empty() || schemes.len() > MAX_IMPORT_SCHEMES {
        return Err(ImportError::InvalidSchemeCount);
    }
    let mut ids = BTreeSet::new();
    let mut exported = Vec::with_capacity(schemes.len());
    for (kind, id) in schemes {
        if !ids.insert(id.clone()) {
            return Err(ImportError::DuplicateId);
        }
        match kind {
            SchemeKind::Chrome => {
                let scheme = catalog.chrome(id).ok_or(ImportError::UnknownScheme)?;
                let mut colors = super::builtin::chrome_base(scheme.appearance);
                colors.apply(&scheme.colors);
                exported.push(CustomScheme::Chrome(Box::new(ChromeScheme {
                    id: scheme.id.clone(),
                    name: scheme.name.clone(),
                    appearance: scheme.appearance,
                    metadata: scheme.metadata.clone(),
                    colors: ChromeColorOverrides::complete(&colors),
                })));
            }
            SchemeKind::Terminal => {
                let scheme = catalog.terminal(id).ok_or(ImportError::UnknownScheme)?;
                let mut colors = super::builtin::terminal_base(scheme.appearance);
                colors.apply(&scheme.colors);
                exported.push(CustomScheme::Terminal(Box::new(TerminalScheme {
                    id: scheme.id.clone(),
                    name: scheme.name.clone(),
                    appearance: scheme.appearance,
                    metadata: scheme.metadata.clone(),
                    colors: TerminalColorOverrides::complete(&colors),
                })));
            }
        }
    }
    export_color_document(&ColorSchemeDocument {
        schema_version: SCHEMA_VERSION,
        schemes: exported,
    })
}

fn validate_color_document(document: &ColorSchemeDocument) -> Result<(), ImportError> {
    if document.schema_version != SCHEMA_VERSION {
        return Err(ImportError::UnsupportedVersion);
    }
    if document.schemes.is_empty() || document.schemes.len() > MAX_IMPORT_SCHEMES {
        return Err(ImportError::InvalidSchemeCount);
    }
    let mut ids = BTreeSet::new();
    for scheme in &document.schemes {
        validate_scheme(scheme, false).map_err(|_| ImportError::InvalidScheme)?;
        if !ids.insert(scheme.id().clone()) {
            return Err(ImportError::DuplicateId);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "native and Zed scheme import are owner operations used by the optional settings UI"
)]
pub(crate) enum ZedImportKind {
    Chrome,
    Terminal,
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

pub(crate) fn import_zed(
    bytes: &[u8],
    candidate_index: usize,
    kinds: &[ZedImportKind],
) -> Result<Vec<CustomScheme>, ImportError> {
    if kinds.is_empty() {
        return Err(ImportError::NoKindSelected);
    }
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
    let hash = format!("{:x}", Sha256::digest(bytes));
    let metadata = SchemeMetadata {
        author: Some(String::from("Imported from Zed")),
        license: None,
        description: Some(String::from("Color roles translated by SpaceTerm")),
    };
    let mut result = Vec::with_capacity(kinds.len());
    let mut seen = BTreeSet::new();
    for kind in kinds {
        if !seen.insert(*kind as u8) {
            continue;
        }
        match kind {
            ZedImportKind::Chrome => result.push(CustomScheme::Chrome(Box::new(ChromeScheme {
                id: SchemeId::new(format!("import.{hash}.{candidate_index}.chrome"))
                    .map_err(|_| ImportError::InvalidScheme)?,
                name: name.to_owned(),
                appearance,
                metadata: metadata.clone(),
                colors: zed_chrome(style)?,
            }))),
            ZedImportKind::Terminal => {
                result.push(CustomScheme::Terminal(Box::new(TerminalScheme {
                    id: SchemeId::new(format!("import.{hash}.{candidate_index}.terminal"))
                        .map_err(|_| ImportError::InvalidScheme)?,
                    name: name.to_owned(),
                    appearance,
                    metadata: metadata.clone(),
                    colors: zed_terminal(style, appearance)?,
                })))
            }
        }
    }
    for scheme in &result {
        validate_scheme(scheme, true).map_err(|_| ImportError::InvalidScheme)?;
    }
    Ok(result)
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
    if name.is_empty() || name.len() > 128 || name.chars().any(char::is_control) {
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

fn zed_chrome(style: &serde_json::Map<String, Value>) -> Result<ChromeColorOverrides, ImportError> {
    let mut colors = ChromeColorOverrides::default();
    macro_rules! map { ($($field:ident => $key:literal),+ $(,)?) => { $(colors.$field = zed_color(style, $key)?;)+ }; }
    map! {
        background => "background", panel_background => "panel.background",
        elevated_surface_background => "elevated_surface.background",
        title_bar_background => "title_bar.background",
        title_bar_inactive_background => "title_bar.inactive_background",
        list_item_background => "element.selected", tab_inactive_background => "tab.inactive_background",
        text => "text", text_muted => "text.muted", text_placeholder => "text.placeholder",
        text_disabled => "text.disabled", text_accent => "text.accent", link_text_hover => "link_text.hover",
        icon => "icon", icon_muted => "icon.muted", icon_disabled => "icon.disabled", icon_accent => "icon.accent",
        border => "border", border_variant => "border.variant", border_focused => "border.focused",
        border_selected => "border.selected", border_disabled => "border.disabled", border_transparent => "border.transparent",
        element_background => "element.background", element_hover => "element.hover", element_active => "element.active",
        element_selected => "element.selected", element_disabled => "element.disabled",
        ghost_element_background => "ghost_element.background", ghost_element_hover => "ghost_element.hover",
        ghost_element_active => "ghost_element.active", ghost_element_selected => "ghost_element.selected",
        ghost_element_disabled => "ghost_element.disabled", info => "info", info_background => "info.background",
        success => "success", warning => "warning", warning_background => "warning.background",
        warning_border => "warning.border", error => "error", error_background => "error.background",
        error_border => "error.border", scrollbar_track_border => "scrollbar.track.border",
        scrollbar_thumb_background => "scrollbar.thumb.background", scrollbar_thumb_border => "scrollbar.thumb.border",
        scrollbar_thumb_hover_background => "scrollbar.thumb.hover_background"
    }
    if let Some(background) = colors.background {
        colors.modal_scrim = Some(Color {
            a: 0x99,
            ..background
        });
    }
    if let Some(players) = style.get("players").and_then(Value::as_array)
        && let Some(selection) = players
            .first()
            .and_then(Value::as_object)
            .and_then(|player| player.get("selection"))
    {
        colors.input_selection_background = match selection {
            Value::String(value) => {
                Some(Color::parse(value).map_err(|()| ImportError::InvalidColor)?)
            }
            Value::Null => None,
            _ => return Err(ImportError::InvalidColor),
        };
    }
    Ok(colors)
}

fn zed_terminal(
    style: &serde_json::Map<String, Value>,
    appearance: Appearance,
) -> Result<TerminalColorOverrides, ImportError> {
    let mut colors = TerminalColorOverrides::default();
    macro_rules! map { ($($field:ident => $key:literal),+ $(,)?) => { $(colors.$field = zed_color(style, $key)?;)+ }; }
    map! { foreground => "terminal.foreground", background => "terminal.background",
    bright_foreground => "terminal.bright_foreground", dim_foreground => "terminal.dim_foreground",
    find_match_background => "search.match_background",
    find_active_match_background => "search.active_match_background", hyperlink => "link_text.hover" }
    let base = super::builtin::terminal_base(appearance);
    colors.normal = zed_palette(
        style,
        "terminal.ansi.",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
        base.normal,
    )?;
    colors.bright = zed_palette(
        style,
        "terminal.ansi.bright_",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
        base.bright,
    )?;
    colors.dim = zed_palette(
        style,
        "terminal.ansi.dim_",
        [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ],
        base.dim,
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
    mut base: [Color; 8],
) -> Result<Option<[Color; 8]>, ImportError> {
    let mut changed = false;
    for (index, name) in names.into_iter().enumerate() {
        if let Some(color) = zed_color(style, &format!("{prefix}{name}"))? {
            base[index] = color;
            changed = true;
        }
    }
    Ok(changed.then_some(base))
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
pub(crate) enum AppearanceDocumentError {
    #[error("appearance document is too large")]
    TooLarge,
    #[error("appearance document is invalid JSON")]
    InvalidJson,
    #[error("appearance document contains a duplicate key")]
    DuplicateKey,
    #[error("appearance document nesting is too deep")]
    TooDeep,
    #[error("appearance document version is unsupported")]
    UnsupportedVersion,
    #[error("appearance preferences are invalid")]
    InvalidPreferences,
    #[error("appearance catalog is invalid")]
    InvalidCatalog,
    #[error("appearance document cannot be serialized")]
    Serialization,
}
impl AppearanceDocumentError {
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
    #[error("no scheme kind was selected")]
    NoKindSelected,
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
