use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::scheme::{CatalogError, TerminalColorOverrides, validate_text};
use super::{Appearance, SchemeId, builtin};

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AppearanceMode {
    Light,
    #[default]
    Dark,
    Auto,
}

impl AppearanceMode {
    pub(crate) const fn resolve(self, system: Appearance) -> Appearance {
        match self {
            Self::Light => Appearance::Light,
            Self::Dark => Appearance::Dark,
            Self::Auto => system,
        }
    }
}

impl From<Appearance> for AppearanceMode {
    fn from(value: Appearance) -> Self {
        match value {
            Appearance::Light => Self::Light,
            Appearance::Dark => Self::Dark,
        }
    }
}

/// Terminal scheme identities for each Appearance Mode slot.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SchemeSlots {
    pub(crate) light: SchemeId,
    pub(crate) dark: SchemeId,
}

impl SchemeSlots {
    pub(crate) fn get(&self, appearance: Appearance) -> &SchemeId {
        match appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
        }
    }

    pub(crate) fn get_mut(&mut self, appearance: Appearance) -> &mut SchemeId {
        match appearance {
            Appearance::Light => &mut self.light,
            Appearance::Dark => &mut self.dark,
        }
    }

    pub(crate) fn set(&mut self, appearance: Appearance, id: SchemeId) {
        *self.get_mut(appearance) = id;
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum TerminalFontFamily {
    DefaultMonospace,
    Named { family: String },
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ChromeDensity {
    #[default]
    Compact,
    Comfortable,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalTypographyPreferences {
    pub(crate) family: TerminalFontFamily,
    pub(crate) base_size: f32,
    pub(crate) regular_weight: u16,
    pub(crate) bold_weight: u16,
    pub(crate) line_height: f32,
    pub(crate) italic: bool,
}

impl Default for TerminalTypographyPreferences {
    fn default() -> Self {
        Self {
            family: TerminalFontFamily::DefaultMonospace,
            base_size: 18.0,
            regular_weight: 400,
            bold_weight: 700,
            line_height: 20.0 / 18.0,
            italic: true,
        }
    }
}

impl TerminalTypographyPreferences {
    pub(crate) fn validate(&self) -> Result<(), PreferenceError> {
        finite_range(self.base_size, 8.0, 32.0)?;
        finite_range(self.line_height, 1.0, 2.0)?;
        valid_weight(self.regular_weight)?;
        valid_weight(self.bold_weight)?;
        if let TerminalFontFamily::Named { family } = &self.family {
            validate_family(family)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalRenderingPreferences {
    pub(crate) bold_as_bright: bool,
}

impl Default for TerminalRenderingPreferences {
    fn default() -> Self {
        Self {
            bold_as_bright: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalPreferences {
    pub(crate) schemes: SchemeSlots,
    pub(crate) typography: TerminalTypographyPreferences,
    pub(crate) rendering: TerminalRenderingPreferences,
    #[serde(default)]
    pub(crate) overrides: BTreeMap<SchemeId, TerminalColorOverrides>,
}

impl Default for TerminalPreferences {
    fn default() -> Self {
        Self {
            schemes: SchemeSlots {
                light: builtin::fallback_id(Appearance::Light),
                dark: builtin::dark_terminal_id(),
            },
            typography: TerminalTypographyPreferences::default(),
            rendering: TerminalRenderingPreferences::default(),
            overrides: BTreeMap::new(),
        }
    }
}

/// Chrome density and window background presentation, independent of Terminal colors.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowPreferences {
    pub(crate) density: ChromeDensity,
    pub(crate) transparency: f32,
    pub(crate) blur: bool,
}

impl Default for WindowPreferences {
    fn default() -> Self {
        Self {
            density: ChromeDensity::Compact,
            transparency: 0.35,
            blur: true,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppearancePreferences {
    pub(crate) mode: AppearanceMode,
    pub(crate) window: WindowPreferences,
    pub(crate) terminal: TerminalPreferences,
}

impl AppearancePreferences {
    pub(crate) fn validate(&self) -> Result<(), PreferenceError> {
        finite_range(self.window.transparency, 0.0, 1.0)?;
        self.terminal.typography.validate()?;
        if self.terminal.overrides.len() > 128 {
            return Err(PreferenceError::TooManyOverrides);
        }
        for overrides in self.terminal.overrides.values() {
            overrides.validate().map_err(|error| match error {
                CatalogError::UnsupportedAlpha => PreferenceError::UnsupportedAlpha,
                _ => PreferenceError::InvalidValue,
            })?;
        }
        Ok(())
    }

    pub(crate) fn reset(&mut self, target: ResetTarget) {
        let defaults = Self::default();
        match target {
            ResetTarget::AppearanceMode => self.mode = defaults.mode,
            ResetTarget::Transparency => self.window.transparency = defaults.window.transparency,
            ResetTarget::Blur => self.window.blur = defaults.window.blur,
            ResetTarget::TerminalScheme(appearance) => {
                *self.terminal.schemes.get_mut(appearance) =
                    defaults.terminal.schemes.get(appearance).clone();
            }
            ResetTarget::TerminalFontFamily => {
                self.terminal.typography.family = defaults.terminal.typography.family
            }
            ResetTarget::TerminalBaseSize => {
                self.terminal.typography.base_size = defaults.terminal.typography.base_size
            }
            ResetTarget::TerminalRegularWeight => {
                self.terminal.typography.regular_weight =
                    defaults.terminal.typography.regular_weight
            }
            ResetTarget::TerminalBoldWeight => {
                self.terminal.typography.bold_weight = defaults.terminal.typography.bold_weight
            }
            ResetTarget::TerminalLineHeight => {
                self.terminal.typography.line_height = defaults.terminal.typography.line_height
            }
            ResetTarget::TerminalItalic => {
                self.terminal.typography.italic = defaults.terminal.typography.italic
            }
            ResetTarget::TerminalBoldAsBright => {
                self.terminal.rendering.bold_as_bright = defaults.terminal.rendering.bold_as_bright
            }
            ResetTarget::TerminalColorOverride { scheme, role } => {
                if let Some(overrides) = self.terminal.overrides.get_mut(&scheme) {
                    overrides.remove_role(role.as_str());
                    if overrides.is_empty() {
                        self.terminal.overrides.remove(&scheme);
                    }
                }
            }
            ResetTarget::Density => self.window.density = defaults.window.density,
            ResetTarget::TerminalColors => {
                self.terminal.schemes = defaults.terminal.schemes;
                self.terminal.overrides.clear();
            }
            ResetTarget::TerminalTypography => {
                self.terminal.typography = defaults.terminal.typography
            }
            ResetTarget::TerminalRendering => self.terminal.rendering = defaults.terminal.rendering,
            ResetTarget::AllAppearance => *self = defaults,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "the Settings Window consumes the field and group targets; the color-role targets await an override editor"
)]
pub(crate) enum ResetTarget {
    AppearanceMode,
    Transparency,
    Blur,
    TerminalScheme(Appearance),
    TerminalFontFamily,
    TerminalBaseSize,
    TerminalRegularWeight,
    TerminalBoldWeight,
    TerminalLineHeight,
    TerminalItalic,
    TerminalBoldAsBright,
    TerminalColorOverride {
        scheme: SchemeId,
        role: TerminalColorRole,
    },
    Density,
    TerminalColors,
    TerminalTypography,
    TerminalRendering,
    AllAppearance,
}

impl ResetTarget {
    #[allow(
        dead_code,
        reason = "individual color-role reset awaits the color override editor"
    )]
    pub(crate) fn terminal_color_override(scheme: SchemeId, role: &'static str) -> Option<Self> {
        Some(Self::TerminalColorOverride {
            scheme,
            role: TerminalColorRole::new(role)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalColorRole(&'static str);

impl TerminalColorRole {
    #[allow(
        dead_code,
        reason = "individual color-role reset awaits the color override editor"
    )]
    pub(crate) fn new(role: &'static str) -> Option<Self> {
        TerminalColorOverrides::supports_role(role).then_some(Self(role))
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

fn validate_family(value: &str) -> Result<(), PreferenceError> {
    validate_text(value, 256).map_err(|_| PreferenceError::InvalidFontFamily)
}

fn finite_range(value: f32, minimum: f32, maximum: f32) -> Result<(), PreferenceError> {
    if value.is_finite() && value >= minimum && value <= maximum {
        Ok(())
    } else {
        Err(PreferenceError::InvalidNumber)
    }
}

fn valid_weight(value: u16) -> Result<(), PreferenceError> {
    if (100..=900).contains(&value) {
        Ok(())
    } else {
        Err(PreferenceError::InvalidWeight)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum PreferenceError {
    #[error("invalid numeric appearance preference")]
    InvalidNumber,
    #[error("invalid font weight")]
    InvalidWeight,
    #[error("invalid font family")]
    InvalidFontFamily,
    #[error("too many color overrides")]
    TooManyOverrides,
    #[error("unsupported alpha")]
    UnsupportedAlpha,
    #[error("invalid appearance preference")]
    InvalidValue,
}
