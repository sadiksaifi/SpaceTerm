use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};

use super::preferences::ResetTarget;
use super::{Appearance, AppearancePreferences, TerminalTheme, ThemeCatalog, ThemeSlots};

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
        self.terminal_themes = defaults.terminal_themes;
    }
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
