//! Terminal color facts. Theme identity and typography stay in the desktop.
pub use crate::color::{Color, ColorParseError};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;
pub fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct AppearanceGeneration(u64);

impl AppearanceGeneration {
    pub const INITIAL: Self = Self(0);
    #[allow(
        dead_code,
        reason = "terminal update boundaries construct externally assigned generations"
    )]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OptionalColorOverride {
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
pub struct TerminalColors {
    pub foreground: Color,
    pub background: Color,
    pub normal: [Color; 8],
    pub bright: [Color; 8],
    pub dim: [Color; 8],
    pub bright_foreground: Color,
    pub dim_foreground: Color,
    pub cursor: Color,
    pub cursor_text: Option<Color>,
    pub selection_background: Color,
    pub selection_foreground: Option<Color>,
    pub find_match_background: Color,
    pub find_match_foreground: Option<Color>,
    pub find_active_match_background: Color,
    pub find_active_match_foreground: Option<Color>,
    pub hyperlink: Color,
    pub visual_bell: Color,
}

impl TerminalColors {
    pub fn apply(&mut self, overrides: &TerminalColorOverrides) {
        macro_rules! apply { ($($field:ident),+ $(,)?) => { $(if let Some(value) = overrides.$field { self.$field = value; })+ }; }
        apply!(
            foreground,
            background,
            bright_foreground,
            dim_foreground,
            cursor,
            selection_background,
            find_match_background,
            find_active_match_background,
            hyperlink,
            visual_bell
        );
        if let Some(palette) = &overrides.normal {
            palette.apply(&mut self.normal);
        }
        if let Some(palette) = &overrides.bright {
            palette.apply(&mut self.bright);
        }
        if let Some(palette) = &overrides.dim {
            palette.apply(&mut self.dim);
        }
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

    pub fn validate(&self) -> Result<(), CatalogError> {
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

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TerminalPaletteOverrides([Option<Color>; 8]);

impl TerminalPaletteOverrides {
    pub fn complete(colors: [Color; 8]) -> Self {
        Self(colors.map(Some))
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn get(&self, index: usize) -> Option<Color> {
        self.0.get(index).copied().flatten()
    }

    fn apply(&self, target: &mut [Color; 8]) {
        for (target, authored) in target.iter_mut().zip(self.0) {
            if let Some(color) = authored {
                *target = color;
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalColorOverrides {
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub background: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub normal: Option<TerminalPaletteOverrides>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub bright: Option<TerminalPaletteOverrides>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub dim: Option<TerminalPaletteOverrides>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub bright_foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub dim_foreground: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub cursor: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub cursor_text: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub selection_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub selection_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub find_match_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub find_match_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub find_active_match_background: Option<Color>,
    #[serde(default, skip_serializing_if = "OptionalColorOverride::is_inherit")]
    pub find_active_match_foreground: OptionalColorOverride,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub hyperlink: Option<Color>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub visual_bell: Option<Color>,
}

impl TerminalColorOverrides {
    pub fn remove_role(&mut self, role: &str) -> bool {
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
    pub fn supports_role(role: &str) -> bool {
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

    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }

    pub fn complete(colors: &TerminalColors) -> Self {
        let optional = |color: Option<Color>| {
            color.map_or(OptionalColorOverride::None, OptionalColorOverride::Color)
        };
        Self {
            foreground: Some(colors.foreground),
            background: Some(colors.background),
            normal: Some(TerminalPaletteOverrides::complete(colors.normal)),
            bright: Some(TerminalPaletteOverrides::complete(colors.bright)),
            dim: Some(TerminalPaletteOverrides::complete(colors.dim)),
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

    pub fn validate(&self) -> Result<(), CatalogError> {
        let mut resolved = TerminalColors::default();
        resolved.apply(self);
        resolved.validate()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CatalogError {
    #[error("invalid theme identity")]
    InvalidId,
    #[error("invalid theme metadata")]
    InvalidMetadata,
    #[error("reserved theme identity")]
    ReservedId,
    #[error("duplicate theme identity")]
    DuplicateId,
    #[error("too many themes")]
    TooManyThemes,
    #[error("theme batch is empty")]
    EmptyBatch,
    #[error("theme is not installed")]
    UnknownTheme,
    #[error("catalog revision conflict")]
    RevisionConflict,
    #[error("catalog revision overflow")]
    RevisionOverflow,
    #[error("unsupported alpha")]
    UnsupportedAlpha,
}
fn spaceterm_dark_terminal() -> TerminalColors {
    TerminalColors {
        foreground: Color::rgb(0xd8d8d8),
        background: Color::rgb(0x191919),
        normal: [
            0x2e2e2e, 0xe5696b, 0x83c07e, 0xe6b85c, 0x5fa3f0, 0xc387d9, 0x5fc0c8, 0xc8c8c8,
        ]
        .map(Color::rgb),
        bright: [
            0x6e6e6e, 0xf28a8b, 0x9fd39a, 0xf0cb80, 0x84b9f6, 0xd5a4e6, 0x82d3d9, 0xf0f0f0,
        ]
        .map(Color::rgb),
        dim: [
            0x1f1f1f, 0xa24c4e, 0x5e895a, 0xa38443, 0x4a78ae, 0x8c64a0, 0x468a90, 0x8a8a8a,
        ]
        .map(Color::rgb),
        bright_foreground: Color::rgb(0xf0f0f0),
        dim_foreground: Color::rgb(0x8a8a8a),
        cursor: Color::rgb(0xd8d8d8),
        cursor_text: None,
        selection_background: Color::rgba(0x34485eaa),
        selection_foreground: None,
        find_match_background: Color::rgba(0xe6b85c4d),
        find_match_foreground: None,
        find_active_match_background: Color::rgba(0xf0913a80),
        find_active_match_foreground: None,
        hyperlink: Color::rgb(0x5ea8ff),
        visual_bell: Color::rgba(0xe6b85c80),
    }
}

impl Default for TerminalColors {
    fn default() -> Self {
        spaceterm_dark_terminal()
    }
}
/// Appearance facts without theme identity or typography.
pub trait TerminalAppearance {
    fn terminal_appearance(&self) -> Appearance;
    fn terminal_colors(&self) -> &TerminalColors;
    fn terminal_bold_as_bright(&self) -> bool;
}
impl<T: TerminalAppearance + ?Sized> TerminalAppearance for Arc<T> {
    fn terminal_appearance(&self) -> Appearance {
        self.as_ref().terminal_appearance()
    }
    fn terminal_colors(&self) -> &TerminalColors {
        self.as_ref().terminal_colors()
    }
    fn terminal_bold_as_bright(&self) -> bool {
        self.as_ref().terminal_bold_as_bright()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTerminalAppearance {
    pub appearance: Appearance,
    pub colors: TerminalColors,
    pub bold_as_bright: bool,
}
impl TerminalAppearance for ResolvedTerminalAppearance {
    fn terminal_appearance(&self) -> Appearance {
        self.appearance
    }
    fn terminal_colors(&self) -> &TerminalColors {
        &self.colors
    }
    fn terminal_bold_as_bright(&self) -> bool {
        self.bold_as_bright
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalAppearanceUpdate {
    pub generation: AppearanceGeneration,
    pub appearance: Arc<ResolvedTerminalAppearance>,
}
impl TerminalAppearanceUpdate {
    pub fn new(
        generation: AppearanceGeneration,
        appearance: Arc<ResolvedTerminalAppearance>,
    ) -> Self {
        Self {
            generation,
            appearance,
        }
    }
}
pub trait AppearanceUpdate {
    fn terminal_update(&self) -> TerminalAppearanceUpdate;
}
impl AppearanceUpdate for TerminalAppearanceUpdate {
    fn terminal_update(&self) -> TerminalAppearanceUpdate {
        self.clone()
    }
}
#[cfg(any(test, feature = "test-support"))]
pub fn test_terminal_appearance_update() -> TerminalAppearanceUpdate {
    TerminalAppearanceUpdate::new(
        AppearanceGeneration::INITIAL,
        Arc::new(ResolvedTerminalAppearance {
            appearance: Appearance::Dark,
            colors: TerminalColors::default(),
            bold_as_bright: true,
        }),
    )
}
