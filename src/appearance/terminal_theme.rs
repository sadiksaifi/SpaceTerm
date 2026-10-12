use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{Color, builtin};

pub(crate) const MAX_THEME_ID_BYTES: usize = 128;
pub(crate) const MAX_THEME_NAME_CHARACTERS: usize = 128;
/// Installed themes stay well inside the Settings Document's size bound.
pub(crate) const MAX_INSTALLED_THEMES: usize = 512;

pub(super) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

pub(crate) use spaceterm_terminal::appearance::Appearance;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ThemeId(String);

impl ThemeId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        if !valid_theme_id(&value) {
            return Err(CatalogError::InvalidId);
        }
        Ok(Self(value))
    }

    pub(crate) fn builtin(value: &'static str) -> Self {
        debug_assert!(valid_theme_id(value));
        Self(value.to_owned())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_reserved(&self) -> bool {
        self.0.starts_with("builtin.")
    }
}

impl fmt::Display for ThemeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for ThemeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ThemeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid theme id"))
    }
}

fn valid_theme_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_THEME_ID_BYTES || !value.is_ascii() {
        return false;
    }
    let mut segments = value.split(['.', '_', '-']);
    let Some(first) = segments.next() else {
        return false;
    };
    if first.is_empty()
        || !first.as_bytes()[0].is_ascii_lowercase()
        || !first
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        return false;
    }
    segments.all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    })
}

/// Where an installed theme came from.
///
/// A theme installed from the Zed extension registry names its extension and version, which is
/// what updating and reinstalling that extension replace. A theme imported from a file names none.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeOrigin {
    pub(crate) format: ThemeSourceFormat,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) package_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) package_version: Option<String>,
    pub(crate) family: String,
    pub(crate) theme: String,
    pub(crate) fingerprint: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ThemeSourceFormat {
    Zed,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ThemeMetadata {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) origin: Option<ThemeOrigin>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) author: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) license: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) description: Option<String>,
}

macro_rules! define_chrome_colors {
    ($($field:ident),+ $(,)?) => {
        #[derive(Clone, Debug, Eq, PartialEq)]
        pub(crate) struct ChromeColors {
            $(pub(crate) $field: Color,)+
        }

    };
}
chrome_color_fields!(define_chrome_colors);

#[cfg(test)]
pub(crate) use spaceterm_terminal::appearance::OptionalColorOverride;
pub(crate) use spaceterm_terminal::appearance::{TerminalColorOverrides, TerminalColors};

/// A Zed extension identity and the version of it that is installed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThemePackage {
    pub(crate) id: String,
    pub(crate) version: String,
}

/// One catalog entry as the theme gallery presents it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThemeSummary {
    pub(crate) id: ThemeId,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    /// Built-in themes cannot be removed, and their identifiers are reserved.
    pub(crate) builtin: bool,
    /// The Zed theme family the theme was published in, when it came from Zed.
    pub(crate) family: Option<String>,
    /// The registry extension that installed this theme, when one did.
    pub(crate) package: Option<ThemePackage>,
    /// The theme's complete harmonized palette, without the person's per-role overrides, for its
    /// preview.
    pub(crate) colors: TerminalColors,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalTheme {
    pub(crate) id: ThemeId,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    #[serde(default, flatten)]
    pub(crate) metadata: ThemeMetadata,
    pub(crate) colors: TerminalColorOverrides,
}

#[derive(Clone, Debug)]
pub(crate) struct ThemeCatalog {
    revision: u64,
    themes: BTreeMap<ThemeId, Arc<TerminalTheme>>,
}

impl Default for ThemeCatalog {
    fn default() -> Self {
        Self {
            revision: 0,
            themes: builtin::builtin_themes()
                .into_iter()
                .map(|theme| (theme.id.clone(), Arc::new(theme)))
                .collect(),
        }
    }
}

impl ThemeCatalog {
    pub(crate) fn from_terminal_themes(themes: &[TerminalTheme]) -> Result<Self, CatalogError> {
        if themes.len() > MAX_INSTALLED_THEMES {
            return Err(CatalogError::TooManyThemes);
        }
        let mut catalog = Self::default();
        let mut seen = BTreeSet::new();
        for theme in themes {
            validate_theme(theme, true)?;
            if !seen.insert(theme.id.clone()) || catalog.contains(&theme.id) {
                return Err(CatalogError::DuplicateId);
            }
            catalog.insert_unchecked(theme.clone());
        }
        Ok(catalog)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Lists installed themes, built-in themes first and the rest by name.
    pub(crate) fn summaries(&self) -> Vec<ThemeSummary> {
        let mut summaries = self
            .themes
            .values()
            .map(|theme| {
                let mut colors = builtin::terminal_base(theme.appearance);
                colors.apply(&theme.colors);
                super::harmonization::harmonize(&mut colors, theme.appearance);
                let origin = theme.metadata.origin.as_ref();
                ThemeSummary {
                    id: theme.id.clone(),
                    name: theme.name.clone(),
                    appearance: theme.appearance,
                    builtin: theme.id.is_reserved(),
                    family: origin.map(|origin| origin.family.clone()),
                    package: origin.and_then(|origin| {
                        Some(ThemePackage {
                            id: origin.package_id.clone()?,
                            version: origin.package_version.clone()?,
                        })
                    }),
                    colors,
                }
            })
            .collect::<Vec<_>>();
        summaries.sort_by_cached_key(|summary| {
            (
                !summary.builtin,
                summary.name.to_lowercase(),
                summary.id.clone(),
            )
        });
        summaries
    }

    pub(crate) fn contains(&self, id: &ThemeId) -> bool {
        self.themes.contains_key(id)
    }
    pub(crate) fn get(&self, id: &ThemeId) -> Option<&TerminalTheme> {
        self.themes.get(id).map(AsRef::as_ref)
    }

    /// Installs a batch atomically, retiring the named installed themes in the same step.
    ///
    /// A theme whose identity is already installed is replaced. Imported identities derive from
    /// their source, so the same identity means the same theme from the same source.
    pub(crate) fn install_batch(
        &mut self,
        themes: &[TerminalTheme],
        expected_revision: u64,
        retire: &BTreeSet<ThemeId>,
    ) -> Result<Vec<ThemeId>, CatalogError> {
        if expected_revision != self.revision {
            return Err(CatalogError::RevisionConflict);
        }
        if themes.is_empty() {
            return Err(CatalogError::EmptyBatch);
        }
        let mut seen = BTreeSet::new();
        for theme in themes {
            validate_theme(theme, true)?;
            if !seen.insert(theme.id.clone()) {
                return Err(CatalogError::DuplicateId);
            }
        }
        for id in retire {
            if id.is_reserved() {
                return Err(CatalogError::ReservedId);
            }
            if !self.contains(id) {
                return Err(CatalogError::UnknownTheme);
            }
        }
        let mut next = self.clone();
        for id in retire.iter().chain(&seen) {
            next.themes.remove(id);
        }
        if next.imported_count().saturating_add(themes.len()) > MAX_INSTALLED_THEMES {
            return Err(CatalogError::TooManyThemes);
        }
        for theme in themes {
            next.insert_unchecked(theme.clone());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(CatalogError::RevisionOverflow)?;
        *self = next;
        Ok(seen.into_iter().collect())
    }

    fn imported_count(&self) -> usize {
        self.themes.keys().filter(|id| !id.is_reserved()).count()
    }

    fn insert_unchecked(&mut self, theme: TerminalTheme) {
        self.themes.insert(theme.id.clone(), Arc::new(theme));
    }
}

pub(super) fn validate_theme(theme: &TerminalTheme, imported: bool) -> Result<(), CatalogError> {
    if imported && theme.id.is_reserved() {
        return Err(CatalogError::ReservedId);
    }
    theme.colors.validate()?;
    let (name, metadata) = (&theme.name, &theme.metadata);
    validate_text(name, MAX_THEME_NAME_CHARACTERS)?;
    if let Some(origin) = &metadata.origin {
        if let Some(id) = &origin.package_id {
            validate_text(id, 256)?;
        }
        if let Some(version) = &origin.package_version {
            validate_text(version, 64)?;
        }
        validate_text(&origin.family, 256)?;
        validate_text(&origin.theme, 128)?;
        if origin.fingerprint.len() != 64
            || !origin
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CatalogError::InvalidMetadata);
        }
    }
    if let Some(value) = &metadata.author {
        validate_text(value, 256)?;
    }
    if let Some(value) = &metadata.license {
        validate_text(value, 256)?;
    }
    if let Some(value) = &metadata.description {
        validate_text(value, 1024)?;
    }
    Ok(())
}

pub(super) fn validate_text(value: &str, max: usize) -> Result<(), CatalogError> {
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        Err(CatalogError::InvalidMetadata)
    } else {
        Ok(())
    }
}

pub(crate) use spaceterm_terminal::appearance::CatalogError;
