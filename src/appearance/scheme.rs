use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use super::{Color, builtin};

pub(crate) const MAX_SCHEME_ID_BYTES: usize = 128;
pub(crate) const MAX_SCHEME_NAME_BYTES: usize = 128;
pub(crate) const MAX_CUSTOM_SCHEMES: usize = 128;

pub(super) fn deserialize_optional_non_null<'de, D, T>(
    deserializer: D,
) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SchemeKind {
    Chrome,
    Terminal,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SchemeId(String);

impl SchemeId {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, CatalogError> {
        let value = value.into();
        if !valid_scheme_id(&value) {
            return Err(CatalogError::InvalidId);
        }
        Ok(Self(value))
    }

    pub(crate) fn builtin(value: &'static str) -> Self {
        debug_assert!(valid_scheme_id(value));
        Self(value.to_owned())
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_reserved(&self) -> bool {
        self.0.starts_with("builtin.")
    }
}

impl fmt::Display for SchemeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Serialize for SchemeId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SchemeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(|_| de::Error::custom("invalid scheme id"))
    }
}

fn valid_scheme_id(value: &str) -> bool {
    if value.is_empty() || value.len() > MAX_SCHEME_ID_BYTES || !value.is_ascii() {
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SchemeMetadata {
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
        #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct ChromeColors {
            $(pub(crate) $field: Color,)+
        }

        #[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
        #[serde(deny_unknown_fields)]
        pub(crate) struct ChromeColorOverrides {
            $(#[serde(default, deserialize_with = "deserialize_optional_non_null", skip_serializing_if = "Option::is_none")]
            pub(crate) $field: Option<Color>,)+
        }

        impl ChromeColors {
            pub(crate) fn apply(&mut self, overrides: &ChromeColorOverrides) {
                $(if let Some(value) = overrides.$field { self.$field = value; })+
            }

            pub(crate) fn validate(&self) -> Result<(), CatalogError> {
                for color in [self.background, self.panel_background,
                    self.elevated_surface_background, self.title_bar_background,
                    self.title_bar_inactive_background, self.tab_active_background,
                    self.tab_inactive_background,
                    self.input_background]
                {
                    if !color.is_opaque() { return Err(CatalogError::UnsupportedAlpha); }
                }
                Ok(())
            }
        }

        impl ChromeColorOverrides {
            pub(crate) fn validate(&self) -> Result<(), CatalogError> {
                for color in [self.background, self.panel_background,
                    self.elevated_surface_background, self.title_bar_background,
                    self.title_bar_inactive_background, self.tab_active_background,
                    self.tab_inactive_background,
                    self.input_background].into_iter().flatten()
                {
                    if !color.is_opaque() { return Err(CatalogError::UnsupportedAlpha); }
                }
                Ok(())
            }

            pub(super) fn remove_role(&mut self, role: &str) -> bool {
                match role {
                    $(stringify!($field) => {
                        self.$field = None;
                        true
                    },)+
                    _ => false,
                }
            }

            #[allow(
                dead_code,
                reason = "validates typed role constructors used by field-reset adapters"
            )]
            pub(super) fn supports_role(role: &str) -> bool {
                matches!(role, $(stringify!($field))|+)
            }

            pub(super) fn is_empty(&self) -> bool {
                self == &Self::default()
            }

            pub(crate) fn complete(colors: &ChromeColors) -> Self {
                Self { $($field: Some(colors.$field),)+ }
            }
        }
    };
}
chrome_color_fields!(define_chrome_colors);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum OptionalColorOverride {
    #[default]
    Inherit,
    None,
    Color(Color),
}

impl OptionalColorOverride {
    fn is_inherit(&self) -> bool {
        matches!(self, Self::Inherit)
    }

    fn apply(self, target: &mut Option<Color>) {
        match self {
            Self::Inherit => {}
            Self::None => *target = None,
            Self::Color(color) => *target = Some(color),
        }
    }
}

impl Serialize for OptionalColorOverride {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::Inherit | Self::None => serializer.serialize_none(),
            Self::Color(color) => color.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for OptionalColorOverride {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Option::<Color>::deserialize(deserializer)
            .map(|value| value.map_or(Self::None, Self::Color))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalColors {
    pub(crate) foreground: Color,
    pub(crate) background: Color,
    pub(crate) normal: [Color; 8],
    pub(crate) bright: [Color; 8],
    pub(crate) dim: [Color; 8],
    pub(crate) bright_foreground: Color,
    pub(crate) dim_foreground: Color,
    pub(crate) cursor: Color,
    pub(crate) cursor_text: Option<Color>,
    pub(crate) selection_background: Color,
    pub(crate) selection_foreground: Option<Color>,
    pub(crate) find_match_background: Color,
    pub(crate) find_match_foreground: Option<Color>,
    pub(crate) find_active_match_background: Color,
    pub(crate) find_active_match_foreground: Option<Color>,
    pub(crate) hyperlink: Color,
    pub(crate) visual_bell: Color,
}

impl TerminalColors {
    pub(crate) fn apply(&mut self, overrides: &TerminalColorOverrides) {
        macro_rules! apply { ($($field:ident),+ $(,)?) => { $(if let Some(value) = overrides.$field { self.$field = value; })+ }; }
        apply!(
            foreground,
            background,
            normal,
            bright,
            dim,
            bright_foreground,
            dim_foreground,
            cursor,
            selection_background,
            find_match_background,
            find_active_match_background,
            hyperlink,
            visual_bell
        );
        overrides.cursor_text.apply(&mut self.cursor_text);
        overrides
            .selection_foreground
            .apply(&mut self.selection_foreground);
        overrides
            .find_match_foreground
            .apply(&mut self.find_match_foreground);
        overrides
            .find_active_match_foreground
            .apply(&mut self.find_active_match_foreground);
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogError> {
        let mut protocol = vec![
            self.foreground,
            self.background,
            self.bright_foreground,
            self.dim_foreground,
            self.cursor,
        ];
        protocol.extend(self.normal);
        protocol.extend(self.bright);
        protocol.extend(self.dim);
        protocol.extend(self.cursor_text);
        protocol.extend(self.selection_foreground);
        protocol.extend(self.find_match_foreground);
        protocol.extend(self.find_active_match_foreground);
        if protocol.into_iter().any(|color| !color.is_opaque()) {
            return Err(CatalogError::UnsupportedAlpha);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalColorOverrides {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) background: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) normal: Option<[Color; 8]>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) bright: Option<[Color; 8]>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) dim: Option<[Color; 8]>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) bright_foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) dim_foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) cursor: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub(crate) cursor_text: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) selection_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub(crate) selection_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) find_match_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub(crate) find_match_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) find_active_match_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub(crate) find_active_match_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) hyperlink: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub(crate) visual_bell: Option<Color>,
}

impl TerminalColorOverrides {
    pub(super) fn remove_role(&mut self, role: &str) -> bool {
        macro_rules! clear_optional {
            ($($field:ident),+ $(,)?) => {
                match role {
                    $(stringify!($field) => {
                        self.$field = None;
                        return true;
                    },)+
                    _ => {}
                }
            };
        }
        clear_optional!(
            foreground,
            background,
            normal,
            bright,
            dim,
            bright_foreground,
            dim_foreground,
            cursor,
            selection_background,
            find_match_background,
            find_active_match_background,
            hyperlink,
            visual_bell,
        );
        match role {
            "cursor_text" => self.cursor_text = OptionalColorOverride::Inherit,
            "selection_foreground" => self.selection_foreground = OptionalColorOverride::Inherit,
            "find_match_foreground" => self.find_match_foreground = OptionalColorOverride::Inherit,
            "find_active_match_foreground" => {
                self.find_active_match_foreground = OptionalColorOverride::Inherit
            }
            _ => return false,
        }
        true
    }

    #[allow(
        dead_code,
        reason = "validates typed role constructors used by field-reset adapters"
    )]
    pub(super) fn supports_role(role: &str) -> bool {
        matches!(
            role,
            "foreground"
                | "background"
                | "normal"
                | "bright"
                | "dim"
                | "bright_foreground"
                | "dim_foreground"
                | "cursor"
                | "cursor_text"
                | "selection_background"
                | "selection_foreground"
                | "find_match_background"
                | "find_match_foreground"
                | "find_active_match_background"
                | "find_active_match_foreground"
                | "hyperlink"
                | "visual_bell"
        )
    }

    pub(super) fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub(crate) fn complete(colors: &TerminalColors) -> Self {
        let optional = |color: Option<Color>| {
            color.map_or(OptionalColorOverride::None, OptionalColorOverride::Color)
        };
        Self {
            foreground: Some(colors.foreground),
            background: Some(colors.background),
            normal: Some(colors.normal),
            bright: Some(colors.bright),
            dim: Some(colors.dim),
            bright_foreground: Some(colors.bright_foreground),
            dim_foreground: Some(colors.dim_foreground),
            cursor: Some(colors.cursor),
            cursor_text: optional(colors.cursor_text),
            selection_background: Some(colors.selection_background),
            selection_foreground: optional(colors.selection_foreground),
            find_match_background: Some(colors.find_match_background),
            find_match_foreground: optional(colors.find_match_foreground),
            find_active_match_background: Some(colors.find_active_match_background),
            find_active_match_foreground: optional(colors.find_active_match_foreground),
            hyperlink: Some(colors.hyperlink),
            visual_bell: Some(colors.visual_bell),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), CatalogError> {
        let mut resolved = builtin::terminal_base(Appearance::Dark);
        resolved.apply(self);
        resolved.validate()
    }
}

/// One catalog entry as a settings list presents it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SchemeSummary {
    pub(crate) id: SchemeId,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    pub(crate) kind: SchemeKind,
    /// Built-in schemes cannot be removed, and their identifiers are reserved.
    pub(crate) builtin: bool,
    /// Representative resolved colors, ordered for a left-to-right preview strip.
    pub(crate) swatches: Vec<Color>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChromeScheme {
    pub(crate) id: SchemeId,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    #[serde(default, flatten)]
    pub(crate) metadata: SchemeMetadata,
    pub(crate) colors: ChromeColorOverrides,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalScheme {
    pub(crate) id: SchemeId,
    pub(crate) name: String,
    pub(crate) appearance: Appearance,
    #[serde(default, flatten)]
    pub(crate) metadata: SchemeMetadata,
    pub(crate) colors: TerminalColorOverrides,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CustomScheme {
    Chrome(Box<ChromeScheme>),
    Terminal(Box<TerminalScheme>),
}

impl CustomScheme {
    pub(crate) fn id(&self) -> &SchemeId {
        match self {
            Self::Chrome(v) => &v.id,
            Self::Terminal(v) => &v.id,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SchemeCatalog {
    revision: u64,
    chrome: BTreeMap<SchemeId, Arc<ChromeScheme>>,
    terminal: BTreeMap<SchemeId, Arc<TerminalScheme>>,
}

impl Default for SchemeCatalog {
    fn default() -> Self {
        Self::new().expect("built-in appearance catalog is valid")
    }
}

impl SchemeCatalog {
    pub(crate) fn new() -> Result<Self, CatalogError> {
        let mut catalog = Self {
            revision: 0,
            chrome: BTreeMap::new(),
            terminal: BTreeMap::new(),
        };
        for scheme in builtin::builtin_schemes() {
            catalog.insert_unchecked(scheme);
        }
        Ok(catalog)
    }

    pub(crate) fn from_custom_schemes(schemes: &[CustomScheme]) -> Result<Self, CatalogError> {
        if schemes.len() > MAX_CUSTOM_SCHEMES {
            return Err(CatalogError::TooManySchemes);
        }
        let mut catalog = Self::new()?;
        let mut seen = BTreeSet::new();
        for scheme in schemes {
            validate_scheme(scheme, true)?;
            if !seen.insert(scheme.id().clone()) || catalog.contains(scheme.id()) {
                return Err(CatalogError::DuplicateId);
            }
            catalog.insert_unchecked(scheme.clone());
        }
        Ok(catalog)
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// Lists one kind's schemes as identity and resolved swatches, without cloning every role.
    ///
    /// A settings surface needs names, appearance, and a small preview. Resolving the complete
    /// color set for every installed scheme to draw a list would be wasteful, so the summary
    /// carries only what a row presents.
    pub(crate) fn summaries(&self, kind: SchemeKind) -> Vec<SchemeSummary> {
        match kind {
            SchemeKind::Chrome => self
                .chrome
                .values()
                .map(|scheme| {
                    let mut colors = builtin::chrome_base(scheme.appearance);
                    colors.apply(&scheme.colors);
                    SchemeSummary {
                        id: scheme.id.clone(),
                        name: scheme.name.clone(),
                        appearance: scheme.appearance,
                        kind,
                        builtin: scheme.id.is_reserved(),
                        swatches: vec![
                            colors.background,
                            colors.panel_background,
                            colors.text,
                            colors.text_accent,
                            colors.border,
                            colors.element_selected,
                        ],
                    }
                })
                .collect(),
            SchemeKind::Terminal => self
                .terminal
                .values()
                .map(|scheme| {
                    let mut colors = builtin::terminal_base(scheme.appearance);
                    colors.apply(&scheme.colors);
                    SchemeSummary {
                        id: scheme.id.clone(),
                        name: scheme.name.clone(),
                        appearance: scheme.appearance,
                        kind,
                        builtin: scheme.id.is_reserved(),
                        swatches: std::iter::once(colors.background)
                            .chain(std::iter::once(colors.foreground))
                            .chain(colors.normal.into_iter().skip(1).take(6))
                            .collect(),
                    }
                })
                .collect(),
        }
    }

    pub(crate) fn schemes(&self) -> Vec<CustomScheme> {
        let mut schemes = self
            .chrome
            .values()
            .map(|scheme| CustomScheme::Chrome(Box::new(scheme.as_ref().clone())))
            .chain(
                self.terminal
                    .values()
                    .map(|scheme| CustomScheme::Terminal(Box::new(scheme.as_ref().clone()))),
            )
            .collect::<Vec<_>>();
        schemes.sort_unstable_by(|left, right| left.id().cmp(right.id()));
        schemes
    }

    pub(crate) fn contains(&self, id: &SchemeId) -> bool {
        self.chrome.contains_key(id) || self.terminal.contains_key(id)
    }
    pub(crate) fn chrome(&self, id: &SchemeId) -> Option<&ChromeScheme> {
        self.chrome.get(id).map(AsRef::as_ref)
    }
    pub(crate) fn terminal(&self, id: &SchemeId) -> Option<&TerminalScheme> {
        self.terminal.get(id).map(AsRef::as_ref)
    }

    pub(crate) fn install_batch(
        &mut self,
        schemes: &[CustomScheme],
        expected_revision: u64,
        replace: &BTreeSet<SchemeId>,
    ) -> Result<Vec<SchemeId>, CatalogError> {
        if expected_revision != self.revision {
            return Err(CatalogError::RevisionConflict);
        }
        if schemes.is_empty() {
            return Err(CatalogError::EmptyBatch);
        }
        if schemes.len() > 32 {
            return Err(CatalogError::TooManySchemes);
        }
        let mut seen = BTreeSet::new();
        for scheme in schemes {
            validate_scheme(scheme, true)?;
            if !seen.insert(scheme.id().clone()) {
                return Err(CatalogError::DuplicateId);
            }
        }
        for id in replace {
            if id.is_reserved() {
                return Err(CatalogError::ReservedId);
            }
            if !seen.contains(id) || !self.contains(id) {
                return Err(CatalogError::UnknownReplacement);
            }
        }
        if schemes
            .iter()
            .any(|scheme| self.contains(scheme.id()) && !replace.contains(scheme.id()))
        {
            return Err(CatalogError::DuplicateId);
        }
        if self
            .custom_count()
            .saturating_sub(replace.len())
            .saturating_add(schemes.len())
            > MAX_CUSTOM_SCHEMES
        {
            return Err(CatalogError::TooManySchemes);
        }

        let mut next = self.clone();
        for scheme in schemes {
            if replace.contains(scheme.id()) {
                next.chrome.remove(scheme.id());
                next.terminal.remove(scheme.id());
            }
            next.insert_unchecked(scheme.clone());
        }
        next.revision = next
            .revision
            .checked_add(1)
            .ok_or(CatalogError::RevisionOverflow)?;
        let installed = schemes.iter().map(|scheme| scheme.id().clone()).collect();
        *self = next;
        Ok(installed)
    }

    fn custom_count(&self) -> usize {
        self.chrome
            .keys()
            .chain(self.terminal.keys())
            .filter(|id| !id.is_reserved())
            .count()
    }

    fn insert_unchecked(&mut self, scheme: CustomScheme) {
        match scheme {
            CustomScheme::Chrome(scheme) => {
                self.chrome.insert(scheme.id.clone(), Arc::from(scheme));
            }
            CustomScheme::Terminal(scheme) => {
                self.terminal.insert(scheme.id.clone(), Arc::from(scheme));
            }
        }
    }
}

pub(super) fn validate_scheme(scheme: &CustomScheme, custom: bool) -> Result<(), CatalogError> {
    if custom && scheme.id().is_reserved() {
        return Err(CatalogError::ReservedId);
    }
    let (name, metadata) = match scheme {
        CustomScheme::Chrome(value) => {
            value.colors.validate()?;
            (&value.name, &value.metadata)
        }
        CustomScheme::Terminal(value) => {
            value.colors.validate()?;
            (&value.name, &value.metadata)
        }
    };
    validate_text(name, MAX_SCHEME_NAME_BYTES)?;
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
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        Err(CatalogError::InvalidMetadata)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CatalogError {
    #[error("invalid scheme identity")]
    InvalidId,
    #[error("invalid scheme metadata")]
    InvalidMetadata,
    #[error("reserved scheme identity")]
    ReservedId,
    #[error("duplicate scheme identity")]
    DuplicateId,
    #[error("too many schemes")]
    TooManySchemes,
    #[error("scheme batch is empty")]
    EmptyBatch,
    #[error("scheme replacement target is missing")]
    UnknownReplacement,
    #[error("catalog revision conflict")]
    RevisionConflict,
    #[error("catalog revision overflow")]
    RevisionOverflow,
    #[error("unsupported alpha")]
    UnsupportedAlpha,
}
