use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};

use super::preferences::ResetTarget;
use super::{Appearance, AppearancePreferences, TerminalTheme, ThemeCatalog, ThemeSlots};
use crate::keybindings::KeybindingPreferences;

const SETTINGS_SCHEMA_VERSION: u32 = 3;
pub(super) const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;
const MAX_DEPTH: usize = 32;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SettingsDocument {
    pub(crate) schema_version: u32,
    pub(crate) revision: u64,
    #[serde(default)]
    pub(crate) updates: crate::updates::policy::UpdatePreferences,
    #[serde(default)]
    pub(crate) keybindings: KeybindingPreferences,
    pub(crate) preferences: AppearancePreferences,
    #[serde(default)]
    pub(crate) terminal_themes: Vec<TerminalTheme>,
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            revision: 0,
            updates: Default::default(),
            keybindings: Default::default(),
            preferences: AppearancePreferences::default(),
            terminal_themes: Vec::new(),
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
        self.keybindings
            .validate()
            .map_err(|_| SettingsDocumentError::InvalidKeybindings)?;
        let catalog = ThemeCatalog::from_terminal_themes(&self.terminal_themes)
            .map_err(|_| SettingsDocumentError::InvalidCatalog)?;
        validate_selection(&catalog, &self.preferences.terminal.themes)?;
        Ok(())
    }

    pub(crate) fn reset(&mut self, target: ResetTarget) -> Result<(), SettingsDocumentError> {
        let mut candidate = self.clone();
        candidate.preferences.reset(target);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Returns every Setting this document owns to its default, imported themes included.
    ///
    /// Preferences and the imported catalog reset together because they constrain each other: a
    /// selection naming an imported theme is only valid while that theme is installed. Clearing
    /// the catalog alone would strand such a selection, and defaulting preferences alone would
    /// leave a library the reset claims to have emptied. The identity fields carry the document
    /// forward instead: `revision` orders the write against concurrent editors, and
    /// `schema_version` states the format this build writes.
    pub(crate) fn reset_all(&mut self) {
        let defaults = Self::default();
        self.preferences = defaults.preferences;
        self.updates = defaults.updates;
        self.keybindings = defaults.keybindings;
        self.terminal_themes = defaults.terminal_themes;
    }

    /// Replaces every Setting and the imported catalog with those of `imported`, keeping this
    /// document's identity fields for the same reason [`Self::reset_all`] does.
    pub(crate) fn replace_settings(&mut self, imported: Self) {
        self.preferences = imported.preferences;
        self.updates = imported.updates;
        self.keybindings = imported.keybindings;
        self.terminal_themes = imported.terminal_themes;
    }

    /// Renders this document's Settings JSON.
    pub(crate) fn settings_json(&self) -> Result<String, SettingsDocumentError> {
        let json = SettingsJsonView {
            updates: &self.updates,
            keybindings: &self.keybindings,
            preferences: &self.preferences,
        };
        serde_json::to_string_pretty(&json)
            .map(|mut output| {
                output.push('\n');
                output
            })
            .map_err(|_| SettingsDocumentError::Serialization)
    }

    /// Replaces every Setting with those that `text`, a Settings JSON, states.
    ///
    /// The text passes the same checks as a Settings Document at load time, and the resulting
    /// document must validate against this document's imported catalog. On any failure the
    /// document is unchanged.
    pub(crate) fn apply_settings_json(&mut self, text: &str) -> Result<(), SettingsJsonError> {
        preflight(text.as_bytes()).map_err(|error| error.locate(text))?;
        let json: SettingsJson = serde_json::from_str(text)
            .map_err(|error| SettingsJsonError::from_serde(&error).locate(text))?;
        let mut candidate = self.clone();
        candidate.updates = json.updates;
        candidate.keybindings = json.keybindings;
        candidate.preferences = json.preferences;
        candidate.validate().map_err(|error| match error {
            SettingsDocumentError::InvalidKeybindings => SettingsJsonError::InvalidKeybindings,
            _ => SettingsJsonError::InvalidPreferences,
        })?;
        *self = candidate;
        Ok(())
    }
}

/// Settings JSON: the Settings a person edits directly, in the order a Settings Document writes
/// them. It leaves out the imported catalog, which is data rather than a Setting, and the identity
/// fields, which SpaceTerm owns.
#[derive(Serialize)]
struct SettingsJsonView<'a> {
    updates: &'a crate::updates::policy::UpdatePreferences,
    keybindings: &'a KeybindingPreferences,
    preferences: &'a AppearancePreferences,
}

/// Settings JSON as read, with the same defaults and strictness as a Settings Document.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsJson {
    #[serde(default)]
    updates: crate::updates::policy::UpdatePreferences,
    #[serde(default)]
    keybindings: KeybindingPreferences,
    preferences: AppearancePreferences,
}

fn validate_selection(
    catalog: &ThemeCatalog,
    slots: &ThemeSlots,
) -> Result<(), SettingsDocumentError> {
    let selections = [
        (&slots.light, Appearance::Light),
        (&slots.dark, Appearance::Dark),
    ];
    for (id, expected) in selections {
        let same = catalog.get(id).map(|theme| theme.appearance);
        if same.is_some_and(|actual| actual != expected) {
            return Err(SettingsDocumentError::InvalidPreferences);
        }
    }
    Ok(())
}

pub(crate) fn parse_settings(bytes: &[u8]) -> Result<SettingsDocument, SettingsDocumentError> {
    preflight(bytes).map_err(SettingsDocumentError::from)?;
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

/// Checks the size, syntax, key uniqueness, and nesting depth that every settings text must meet
/// before its structure is read.
///
/// Positions are in bytes. [`SettingsJsonError::locate`] converts them to characters.
fn preflight(bytes: &[u8]) -> Result<(), SettingsJsonError> {
    if bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(SettingsJsonError::TooLarge);
    }
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    DepthSeed(0)
        .deserialize(&mut deserializer)
        .map_err(|error| {
            let at = JsonPosition::of(&error);
            if error.to_string().contains(DUPLICATE_KEY) {
                SettingsJsonError::DuplicateKey(at)
            } else if error.to_string().contains(NESTING_DEPTH) {
                SettingsJsonError::TooDeep(at)
            } else {
                SettingsJsonError::from_serde(&error)
            }
        })?;
    deserializer
        .end()
        .map_err(|error| SettingsJsonError::from_serde(&error))
}

const DUPLICATE_KEY: &str = "duplicate key";
const NESTING_DEPTH: &str = "nesting depth exceeded";

struct DepthSeed(usize);
impl<'de> DeserializeSeed<'de> for DepthSeed {
    type Value = ();
    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.0 > MAX_DEPTH {
            return Err(de::Error::custom(NESTING_DEPTH));
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
                return Err(de::Error::custom(DUPLICATE_KEY));
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
    #[error("keybinding preferences are invalid")]
    InvalidKeybindings,
    #[error("appearance catalog is invalid")]
    InvalidCatalog,
    #[error("settings document cannot be serialized")]
    Serialization,
}
impl From<SettingsJsonError> for SettingsDocumentError {
    fn from(error: SettingsJsonError) -> Self {
        match error {
            SettingsJsonError::TooLarge => Self::TooLarge,
            SettingsJsonError::DuplicateKey(_) => Self::DuplicateKey,
            SettingsJsonError::TooDeep(_) => Self::TooDeep,
            SettingsJsonError::UnexpectedEnd
            | SettingsJsonError::Syntax(_)
            | SettingsJsonError::Structure(_) => Self::InvalidJson,
            SettingsJsonError::InvalidPreferences => Self::InvalidPreferences,
            SettingsJsonError::InvalidKeybindings => Self::InvalidKeybindings,
        }
    }
}

/// Where a fault in settings text was detected, as a one-based line and column.
///
/// The column counts characters, so it matches what a person sees in the text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct JsonPosition {
    pub(crate) line: usize,
    pub(crate) column: usize,
}

impl JsonPosition {
    /// The parser's position, whose column counts bytes.
    fn of(error: &serde_json::Error) -> Self {
        Self {
            line: error.line(),
            column: error.column(),
        }
    }

    /// Converts a byte column to a character column within `text`.
    fn in_characters(self, text: &str) -> Self {
        let Some(line) = text.split('\n').nth(self.line.saturating_sub(1)) else {
            return self;
        };
        let bytes = self.column.min(line.len());
        let column = line
            .char_indices()
            .take_while(|(offset, _)| *offset < bytes)
            .count()
            .max(1);
        Self {
            line: self.line,
            column,
        }
    }
}

/// Why Settings JSON could not be applied. Nothing here carries the text itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum SettingsJsonError {
    #[error("settings JSON is too large")]
    TooLarge,
    #[error("settings JSON ends before it is complete")]
    UnexpectedEnd,
    #[error("settings JSON has a syntax error")]
    Syntax(JsonPosition),
    #[error("settings JSON contains a duplicate key")]
    DuplicateKey(JsonPosition),
    #[error("settings JSON nesting is too deep")]
    TooDeep(JsonPosition),
    #[error("settings JSON does not match the settings format")]
    Structure(JsonPosition),
    #[error("appearance preferences are invalid")]
    InvalidPreferences,
    #[error("keybinding preferences are invalid")]
    InvalidKeybindings,
}

impl SettingsJsonError {
    fn from_serde(error: &serde_json::Error) -> Self {
        use serde_json::error::Category;
        let at = JsonPosition::of(error);
        match error.classify() {
            Category::Eof => Self::UnexpectedEnd,
            Category::Syntax | Category::Io => Self::Syntax(at),
            Category::Data => Self::Structure(at),
        }
    }

    /// Where the fault was detected, when it has a place in the text.
    pub(crate) fn position(self) -> Option<JsonPosition> {
        match self {
            Self::Syntax(at) | Self::DuplicateKey(at) | Self::TooDeep(at) | Self::Structure(at) => {
                Some(at)
            }
            Self::TooLarge
            | Self::UnexpectedEnd
            | Self::InvalidPreferences
            | Self::InvalidKeybindings => None,
        }
    }

    fn locate(self, text: &str) -> Self {
        match self {
            Self::Syntax(at) => Self::Syntax(at.in_characters(text)),
            Self::DuplicateKey(at) => Self::DuplicateKey(at.in_characters(text)),
            Self::TooDeep(at) => Self::TooDeep(at.in_characters(text)),
            Self::Structure(at) => Self::Structure(at.in_characters(text)),
            other => other,
        }
    }
}
