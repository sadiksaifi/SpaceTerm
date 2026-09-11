use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::scheme::{CatalogError, ChromeColorOverrides, TerminalColorOverrides, validate_text};
use super::{Appearance, SchemeId, builtin};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum SchemeSelection {
    Fixed {
        id: SchemeId,
        appearance: Appearance,
    },
    System {
        light: SchemeId,
        dark: SchemeId,
    },
}

impl SchemeSelection {
    pub(crate) fn select(&self, system: Appearance) -> (&SchemeId, Appearance) {
        match self {
            Self::Fixed { id, appearance } => (id, *appearance),
            Self::System { light, dark } => match system {
                Appearance::Light => (light, Appearance::Light),
                Appearance::Dark => (dark, Appearance::Dark),
            },
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ChromeFontFamily {
    SystemUi,
    Named { family: String },
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
pub(crate) struct ChromeTypographyPreferences {
    pub(crate) family: ChromeFontFamily,
    pub(crate) base_size: f32,
    pub(crate) regular_weight: u16,
    pub(crate) emphasis_weight: u16,
    pub(crate) heading_weight: u16,
}

impl Default for ChromeTypographyPreferences {
    fn default() -> Self {
        Self {
            family: ChromeFontFamily::SystemUi,
            base_size: 13.0,
            regular_weight: 400,
            emphasis_weight: 600,
            heading_weight: 600,
        }
    }
}

impl ChromeTypographyPreferences {
    pub(crate) fn validate(&self) -> Result<(), PreferenceError> {
        finite_range(self.base_size, 10.0, 24.0)?;
        for weight in [
            self.regular_weight,
            self.emphasis_weight,
            self.heading_weight,
        ] {
            valid_weight(weight)?;
        }
        if let ChromeFontFamily::Named { family } = &self.family {
            validate_family(family)?;
        }
        Ok(())
    }
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
pub(crate) struct ChromePreferences {
    pub(crate) scheme: SchemeSelection,
    pub(crate) typography: ChromeTypographyPreferences,
    pub(crate) density: ChromeDensity,
    #[serde(default)]
    pub(crate) overrides: BTreeMap<SchemeId, ChromeColorOverrides>,
}

impl Default for ChromePreferences {
    fn default() -> Self {
        Self {
            scheme: SchemeSelection::Fixed {
                id: builtin::vague_chrome_id(),
                appearance: Appearance::Dark,
            },
            typography: ChromeTypographyPreferences::default(),
            density: ChromeDensity::Compact,
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TerminalPreferences {
    pub(crate) scheme: SchemeSelection,
    pub(crate) typography: TerminalTypographyPreferences,
    pub(crate) rendering: TerminalRenderingPreferences,
    #[serde(default)]
    pub(crate) overrides: BTreeMap<SchemeId, TerminalColorOverrides>,
}

impl Default for TerminalPreferences {
    fn default() -> Self {
        Self {
            scheme: SchemeSelection::Fixed {
                id: builtin::vague_terminal_id(),
                appearance: Appearance::Dark,
            },
            typography: TerminalTypographyPreferences::default(),
            rendering: TerminalRenderingPreferences::default(),
            overrides: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppearancePreferences {
    pub(crate) chrome: ChromePreferences,
    pub(crate) terminal: TerminalPreferences,
}

impl AppearancePreferences {
    pub(crate) fn validate(&self) -> Result<(), PreferenceError> {
        self.chrome.typography.validate()?;
        self.terminal.typography.validate()?;
        if self.chrome.overrides.len() > 128 || self.terminal.overrides.len() > 128 {
            return Err(PreferenceError::TooManyOverrides);
        }
        for overrides in self.chrome.overrides.values() {
            overrides.validate().map_err(|error| match error {
                CatalogError::UnsupportedAlpha => PreferenceError::UnsupportedAlpha,
                _ => PreferenceError::InvalidValue,
            })?;
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
            ResetTarget::ChromeSchemeSelection => self.chrome.scheme = defaults.chrome.scheme,
            ResetTarget::ChromeFontFamily => {
                self.chrome.typography.family = defaults.chrome.typography.family
            }
            ResetTarget::ChromeBaseSize => {
                self.chrome.typography.base_size = defaults.chrome.typography.base_size
            }
            ResetTarget::ChromeRegularWeight => {
                self.chrome.typography.regular_weight = defaults.chrome.typography.regular_weight
            }
            ResetTarget::ChromeEmphasisWeight => {
                self.chrome.typography.emphasis_weight = defaults.chrome.typography.emphasis_weight
            }
            ResetTarget::ChromeHeadingWeight => {
                self.chrome.typography.heading_weight = defaults.chrome.typography.heading_weight
            }
            ResetTarget::TerminalSchemeSelection => self.terminal.scheme = defaults.terminal.scheme,
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
            ResetTarget::ChromeColorOverride { scheme, role } => {
                if let Some(overrides) = self.chrome.overrides.get_mut(&scheme) {
                    overrides.remove_role(role.as_str());
                    if overrides.is_empty() {
                        self.chrome.overrides.remove(&scheme);
                    }
                }
            }
            ResetTarget::TerminalColorOverride { scheme, role } => {
                if let Some(overrides) = self.terminal.overrides.get_mut(&scheme) {
                    overrides.remove_role(role.as_str());
                    if overrides.is_empty() {
                        self.terminal.overrides.remove(&scheme);
                    }
                }
            }
            ResetTarget::ChromeColors => {
                self.chrome.scheme = defaults.chrome.scheme;
                self.chrome.overrides.clear();
            }
            ResetTarget::ChromeTypography => self.chrome.typography = defaults.chrome.typography,
            ResetTarget::ChromeDensity => self.chrome.density = defaults.chrome.density,
            ResetTarget::TerminalColors => {
                self.terminal.scheme = defaults.terminal.scheme;
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
    reason = "the complete typed reset surface is consumed by the optional settings UI"
)]
pub(crate) enum ResetTarget {
    ChromeSchemeSelection,
    ChromeFontFamily,
    ChromeBaseSize,
    ChromeRegularWeight,
    ChromeEmphasisWeight,
    ChromeHeadingWeight,
    TerminalSchemeSelection,
    TerminalFontFamily,
    TerminalBaseSize,
    TerminalRegularWeight,
    TerminalBoldWeight,
    TerminalLineHeight,
    TerminalItalic,
    TerminalBoldAsBright,
    ChromeColorOverride {
        scheme: SchemeId,
        role: ChromeColorRole,
    },
    TerminalColorOverride {
        scheme: SchemeId,
        role: TerminalColorRole,
    },
    ChromeColors,
    ChromeTypography,
    ChromeDensity,
    TerminalColors,
    TerminalTypography,
    TerminalRendering,
    AllAppearance,
}

impl ResetTarget {
    #[allow(
        dead_code,
        reason = "individual color-role reset is consumed by the optional settings UI"
    )]
    pub(crate) fn chrome_color_override(scheme: SchemeId, role: &'static str) -> Option<Self> {
        Some(Self::ChromeColorOverride {
            scheme,
            role: ChromeColorRole::new(role)?,
        })
    }

    #[allow(
        dead_code,
        reason = "individual color-role reset is consumed by the optional settings UI"
    )]
    pub(crate) fn terminal_color_override(scheme: SchemeId, role: &'static str) -> Option<Self> {
        Some(Self::TerminalColorOverride {
            scheme,
            role: TerminalColorRole::new(role)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ChromeColorRole(&'static str);

impl ChromeColorRole {
    #[allow(
        dead_code,
        reason = "individual color-role reset is consumed by the optional settings UI"
    )]
    pub(crate) fn new(role: &'static str) -> Option<Self> {
        ChromeColorOverrides::supports_role(role).then_some(Self(role))
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TerminalColorRole(&'static str);

impl TerminalColorRole {
    #[allow(
        dead_code,
        reason = "individual color-role reset is consumed by the optional settings UI"
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
