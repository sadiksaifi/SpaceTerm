use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};

use crate::appearance::{
    Appearance, AppearancePreferences, ResetTarget, TerminalTheme, ThemeCatalog, ThemeSlots,
};
use crate::keybindings::KeybindingPreferences;

const SETTINGS_SCHEMA_VERSION: u32 = 6;
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
    #[serde(default)]
    pub(crate) clipboard: crate::terminal::native_services::clipboard::ClipboardPreferences,
    #[serde(default)]
    pub(crate) git: super::git::GitPreferences,
    pub(crate) appearance: AppearancePreferences,
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
            clipboard: Default::default(),
            git: Default::default(),
            appearance: AppearancePreferences::default(),
            terminal_themes: Vec::new(),
        }
    }
}

impl SettingsDocument {
    pub(crate) fn validate(&self) -> Result<(), SettingsDocumentError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsDocumentError::UnsupportedVersion);
        }
        self.appearance
            .validate()
            .map_err(|_| SettingsDocumentError::InvalidAppearance)?;
        self.keybindings
            .validate()
            .map_err(|_| SettingsDocumentError::InvalidKeybindings)?;
        self.git
            .validate()
            .map_err(|_| SettingsDocumentError::InvalidGit)?;
        let catalog = ThemeCatalog::from_terminal_themes(&self.terminal_themes)
            .map_err(|_| SettingsDocumentError::InvalidCatalog)?;
        validate_selection(&catalog, &self.appearance.terminal.themes)?;
        Ok(())
    }

    /// Returns each Terminal Theme slot that names an uninstalled theme to the built-in theme for
    /// its appearance, so every selection names a theme terminal panes can draw.
    pub(crate) fn select_builtin_for_missing_themes(&mut self) {
        let Ok(catalog) = ThemeCatalog::from_terminal_themes(&self.terminal_themes) else {
            return;
        };
        for slot in [Appearance::Light, Appearance::Dark] {
            if catalog
                .get(self.appearance.terminal.themes.get(slot))
                .is_none()
            {
                self.appearance.reset(ResetTarget::TerminalTheme(slot));
            }
        }
    }

    pub(crate) fn reset(&mut self, target: ResetTarget) -> Result<(), SettingsDocumentError> {
        let mut candidate = self.clone();
        candidate.appearance.reset(target);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    /// Resets preferences and imported themes together, preserving revision and schema identity.
    pub(crate) fn reset_all(&mut self) {
        let defaults = Self::default();
        self.appearance = defaults.appearance;
        self.updates = defaults.updates;
        self.keybindings = defaults.keybindings;
        self.clipboard = defaults.clipboard;
        self.git = defaults.git;
        self.terminal_themes = defaults.terminal_themes;
    }

    /// Replaces every Setting and the imported catalog with those of `imported`, keeping this
    /// document's identity fields for the same reason [`Self::reset_all`] does.
    pub(crate) fn replace_settings(&mut self, imported: Self) {
        self.appearance = imported.appearance;
        self.updates = imported.updates;
        self.keybindings = imported.keybindings;
        self.clipboard = imported.clipboard;
        self.git = imported.git;
        self.terminal_themes = imported.terminal_themes;
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
        if catalog.get(id).map(|theme| theme.appearance) != Some(expected) {
            return Err(SettingsDocumentError::InvalidAppearance);
        }
    }
    Ok(())
}

pub(crate) fn parse_settings(bytes: &[u8]) -> Result<SettingsDocument, SettingsDocumentError> {
    preflight(bytes).map_err(SettingsDocumentError::from_preflight)?;
    let mut value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| SettingsDocumentError::InvalidJson)?;
    match value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
    {
        Some(3) => {
            migrate_v3_settings(&mut value)?;
            migrate_v4_settings(&mut value)?;
        }
        Some(4) => migrate_v4_settings(&mut value)?,
        Some(5) => migrate_v5_settings(&mut value)?,
        Some(version) if version == u64::from(SETTINGS_SCHEMA_VERSION) => {}
        Some(_) => return Err(SettingsDocumentError::UnsupportedVersion),
        None => return Err(SettingsDocumentError::InvalidJson),
    }
    let mut document: SettingsDocument =
        serde_json::from_value(value).map_err(|_| SettingsDocumentError::InvalidJson)?;
    document.select_builtin_for_missing_themes();
    document.validate()?;
    Ok(document)
}

fn migrate_v3_settings(value: &mut serde_json::Value) -> Result<(), SettingsDocumentError> {
    let window = value
        .pointer_mut("/appearance/window")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(SettingsDocumentError::InvalidJson)?;
    if window.contains_key("opacity") {
        return Err(SettingsDocumentError::InvalidJson);
    }
    let transparency: f64 = serde_json::from_value(
        window
            .remove("transparency")
            .ok_or(SettingsDocumentError::InvalidJson)?,
    )
    .map_err(|_| SettingsDocumentError::InvalidJson)?;
    if !transparency.is_finite() || !(0.0..=1.0).contains(&transparency) {
        return Err(SettingsDocumentError::InvalidAppearance);
    }
    window.insert("opacity".into(), serde_json::json!(1.0 - transparency));
    value["schema_version"] = serde_json::json!(4);
    Ok(())
}

/// Version 5 adds the `git` preferences, which a version 4 document takes at their defaults.
fn migrate_v4_settings(value: &mut serde_json::Value) -> Result<(), SettingsDocumentError> {
    let document = value
        .as_object_mut()
        .ok_or(SettingsDocumentError::InvalidJson)?;
    if document.contains_key("git") {
        return Err(SettingsDocumentError::InvalidJson);
    }
    document.insert(
        "schema_version".into(),
        serde_json::json!(SETTINGS_SCHEMA_VERSION),
    );
    Ok(())
}

/// Version 6 adds `git.worktree_path_template`, which a version 5 document takes at its default.
/// The version moves so that an older SpaceTerm reports a newer document as unsupported instead
/// of malformed.
fn migrate_v5_settings(value: &mut serde_json::Value) -> Result<(), SettingsDocumentError> {
    let document = value
        .as_object_mut()
        .ok_or(SettingsDocumentError::InvalidJson)?;
    if document
        .get("git")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|git| git.contains_key("worktree_path_template"))
    {
        return Err(SettingsDocumentError::InvalidJson);
    }
    document.insert(
        "schema_version".into(),
        serde_json::json!(SETTINGS_SCHEMA_VERSION),
    );
    Ok(())
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
    InvalidAppearance,
    #[error("keybinding preferences are invalid")]
    InvalidKeybindings,
    #[error("git preferences are invalid")]
    InvalidGit,
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
