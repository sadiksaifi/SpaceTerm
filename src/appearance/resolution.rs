use std::sync::Arc;

use super::{
    Appearance, AppearancePreferences, ChromeColors, ChromeDensity, ChromeFontFamily,
    SchemeCatalog, SchemeId, SchemeKind, TerminalColors, TerminalFontFamily, builtin,
    preferences::PreferenceError,
};

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct AppearanceGeneration(u64);

impl AppearanceGeneration {
    pub(crate) const INITIAL: Self = Self(0);
    #[allow(
        dead_code,
        reason = "terminal update boundaries construct externally assigned generations"
    )]
    pub(crate) const fn new(value: u64) -> Self {
        Self(value)
    }
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
    pub(crate) fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SystemAppearance(Option<Appearance>);

impl SystemAppearance {
    #[cfg(test)]
    pub(crate) const fn available(appearance: Appearance) -> Self {
        Self(Some(appearance))
    }
    pub(crate) const fn unavailable() -> Self {
        Self(None)
    }
    pub(crate) const fn effective(self) -> Appearance {
        match self.0 {
            Some(appearance) => appearance,
            None => Appearance::Dark,
        }
    }
}

impl From<Option<Appearance>> for SystemAppearance {
    fn from(value: Option<Appearance>) -> Self {
        Self(value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontClass {
    Proportional,
    Monospace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AvailableFont {
    pub(crate) family: String,
    pub(crate) class: FontClass,
    pub(crate) resolution_identity: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AvailableFonts {
    pub(crate) system_ui: AvailableFont,
    pub(crate) system_monospace: AvailableFont,
    pub(crate) installed: Vec<AvailableFont>,
}

impl Default for AvailableFonts {
    fn default() -> Self {
        Self {
            system_ui: AvailableFont {
                family: String::from("system-ui"),
                class: FontClass::Proportional,
                resolution_identity: String::from("system-ui"),
            },
            system_monospace: AvailableFont {
                family: String::from("monospace"),
                class: FontClass::Monospace,
                resolution_identity: String::from("system-monospace"),
            },
            installed: Vec::new(),
        }
    }
}

impl AvailableFonts {
    fn named(&self, family: &str) -> Option<&AvailableFont> {
        self.installed.iter().find(|font| font.family == family)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FontStyle {
    Normal,
    Italic,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedFontDescriptor {
    pub(crate) primary_family: String,
    pub(crate) fallback_families: Vec<String>,
    pub(crate) size: f32,
    pub(crate) line_height: f32,
    pub(crate) weight: u16,
    pub(crate) style: FontStyle,
    pub(crate) features: Vec<String>,
    pub(crate) resolution_identity: String,
}

impl ResolvedFontDescriptor {
    fn with_role(&self, size: f32, weight: u16) -> Self {
        Self {
            size,
            line_height: size,
            weight,
            ..self.clone()
        }
    }

    fn with_style(&self, weight: u16, style: FontStyle) -> Self {
        Self {
            weight,
            style,
            ..self.clone()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedChromeTypography {
    pub(crate) body: ResolvedFontDescriptor,
    pub(crate) small: ResolvedFontDescriptor,
    pub(crate) control: ResolvedFontDescriptor,
    pub(crate) navigation: ResolvedFontDescriptor,
    pub(crate) caption: ResolvedFontDescriptor,
    pub(crate) heading: ResolvedFontDescriptor,
    pub(crate) shortcut: ResolvedFontDescriptor,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedTerminalTypography {
    pub(crate) regular: ResolvedFontDescriptor,
    pub(crate) bold: ResolvedFontDescriptor,
    pub(crate) italic: ResolvedFontDescriptor,
    pub(crate) bold_italic: ResolvedFontDescriptor,
    pub(crate) cell_size: f32,
    pub(crate) line_height: f32,
    pub(crate) render_italic: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedChromeAppearance {
    pub(crate) requested_scheme: SchemeId,
    pub(crate) effective_scheme: SchemeId,
    pub(crate) appearance: Appearance,
    pub(crate) colors: ChromeColors,
    pub(crate) typography: ResolvedChromeTypography,
    pub(crate) density: ChromeDensity,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedTerminalAppearance {
    pub(crate) requested_scheme: SchemeId,
    pub(crate) effective_scheme: SchemeId,
    pub(crate) appearance: Appearance,
    pub(crate) colors: TerminalColors,
    pub(crate) typography: ResolvedTerminalTypography,
    pub(crate) bold_as_bright: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppearanceDiagnostic {
    SystemAppearanceUnavailable,
    ChromeSchemeUnavailable { appearance: Appearance },
    TerminalSchemeUnavailable { appearance: Appearance },
    ChromeFontUnavailable,
    TerminalFontUnavailable,
    TerminalFontNotMonospace,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedAppearance {
    pub(crate) generation: AppearanceGeneration,
    pub(crate) chrome: Arc<ResolvedChromeAppearance>,
    pub(crate) terminal: Arc<ResolvedTerminalAppearance>,
    pub(crate) diagnostics: Vec<AppearanceDiagnostic>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct AppearanceChangeSet {
    pub(crate) chrome_colors: bool,
    pub(crate) chrome_typography: bool,
    pub(crate) chrome_metrics: bool,
    pub(crate) terminal_protocol_colors: bool,
    pub(crate) terminal_interaction_colors: bool,
    pub(crate) terminal_typography: bool,
    pub(crate) terminal_rendering: bool,
}

impl AppearanceChangeSet {
    pub(crate) fn between(previous: &ResolvedAppearance, next: &ResolvedAppearance) -> Self {
        Self {
            chrome_colors: previous.chrome.colors != next.chrome.colors
                || previous.chrome.appearance != next.chrome.appearance,
            chrome_typography: previous.chrome.typography != next.chrome.typography,
            chrome_metrics: previous.chrome.typography != next.chrome.typography
                || previous.chrome.density != next.chrome.density,
            terminal_protocol_colors: protocol_colors(&previous.terminal.colors)
                != protocol_colors(&next.terminal.colors)
                || previous.terminal.appearance != next.terminal.appearance,
            terminal_interaction_colors: interaction_colors(&previous.terminal.colors)
                != interaction_colors(&next.terminal.colors),
            terminal_typography: previous.terminal.typography != next.terminal.typography,
            terminal_rendering: previous.terminal.bold_as_bright != next.terminal.bold_as_bright,
        }
    }
}

impl SchemeCatalog {
    pub(crate) fn resolve(
        &self,
        generation: AppearanceGeneration,
        preferences: &AppearancePreferences,
        system: SystemAppearance,
        fonts: &AvailableFonts,
    ) -> Result<ResolvedAppearance, ResolutionError> {
        preferences
            .validate()
            .map_err(ResolutionError::Preferences)?;
        let mut diagnostics = Vec::new();
        if system.0.is_none() {
            diagnostics.push(AppearanceDiagnostic::SystemAppearanceUnavailable);
        }
        let system = system.effective();
        let (requested_chrome, chrome_appearance) = preferences.chrome.scheme.select(system);
        let (requested_terminal, terminal_appearance) = preferences.terminal.scheme.select(system);
        let (effective_chrome, mut chrome_colors, found_chrome) =
            self.resolve_chrome_scheme(requested_chrome, chrome_appearance)?;
        if !found_chrome {
            diagnostics.push(AppearanceDiagnostic::ChromeSchemeUnavailable {
                appearance: chrome_appearance,
            });
        }
        if found_chrome && let Some(overrides) = preferences.chrome.overrides.get(requested_chrome)
        {
            chrome_colors.apply(overrides);
        }
        chrome_colors
            .validate()
            .map_err(|_| ResolutionError::UnsupportedAlpha)?;
        let (effective_terminal, mut terminal_colors, found_terminal) =
            self.resolve_terminal_scheme(requested_terminal, terminal_appearance)?;
        if !found_terminal {
            diagnostics.push(AppearanceDiagnostic::TerminalSchemeUnavailable {
                appearance: terminal_appearance,
            });
        }
        if found_terminal
            && let Some(overrides) = preferences.terminal.overrides.get(requested_terminal)
        {
            terminal_colors.apply(overrides);
        }
        terminal_colors
            .validate()
            .map_err(|_| ResolutionError::UnsupportedAlpha)?;
        let chrome_typography = resolve_chrome_typography(preferences, fonts, &mut diagnostics);
        let terminal_typography = resolve_terminal_typography(preferences, fonts, &mut diagnostics);
        Ok(ResolvedAppearance {
            generation,
            chrome: Arc::new(ResolvedChromeAppearance {
                requested_scheme: requested_chrome.clone(),
                effective_scheme: effective_chrome,
                appearance: chrome_appearance,
                colors: chrome_colors,
                typography: chrome_typography,
                density: preferences.chrome.density,
            }),
            terminal: Arc::new(ResolvedTerminalAppearance {
                requested_scheme: requested_terminal.clone(),
                effective_scheme: effective_terminal,
                appearance: terminal_appearance,
                colors: terminal_colors,
                typography: terminal_typography,
                bold_as_bright: preferences.terminal.rendering.bold_as_bright,
            }),
            diagnostics,
        })
    }

    fn resolve_chrome_scheme(
        &self,
        requested: &SchemeId,
        appearance: Appearance,
    ) -> Result<(SchemeId, ChromeColors, bool), ResolutionError> {
        if let Some(scheme) = self.chrome(requested) {
            if scheme.appearance != appearance {
                return Err(ResolutionError::AppearanceMismatch);
            }
            let mut colors = builtin::chrome_base(appearance);
            colors.apply(&scheme.colors);
            return Ok((requested.clone(), colors, true));
        }
        if self.terminal(requested).is_some() {
            return Err(ResolutionError::WrongSchemeKind);
        }
        let id = builtin::fallback_id(SchemeKind::Chrome, appearance);
        Ok((id, builtin::chrome_base(appearance), false))
    }

    fn resolve_terminal_scheme(
        &self,
        requested: &SchemeId,
        appearance: Appearance,
    ) -> Result<(SchemeId, TerminalColors, bool), ResolutionError> {
        if let Some(scheme) = self.terminal(requested) {
            if scheme.appearance != appearance {
                return Err(ResolutionError::AppearanceMismatch);
            }
            let mut colors = builtin::terminal_base(appearance);
            colors.apply(&scheme.colors);
            return Ok((requested.clone(), colors, true));
        }
        if self.chrome(requested).is_some() {
            return Err(ResolutionError::WrongSchemeKind);
        }
        let id = builtin::fallback_id(SchemeKind::Terminal, appearance);
        Ok((id, builtin::terminal_base(appearance), false))
    }
}

fn resolve_chrome_typography(
    preferences: &AppearancePreferences,
    fonts: &AvailableFonts,
    diagnostics: &mut Vec<AppearanceDiagnostic>,
) -> ResolvedChromeTypography {
    let requested = &preferences.chrome.typography;
    let font = match &requested.family {
        ChromeFontFamily::SystemUi => &fonts.system_ui,
        ChromeFontFamily::Named { family } => fonts.named(family).unwrap_or_else(|| {
            diagnostics.push(AppearanceDiagnostic::ChromeFontUnavailable);
            &fonts.system_ui
        }),
    };
    let base = descriptor(
        font,
        requested.base_size,
        requested.regular_weight,
        FontStyle::Normal,
        Vec::new(),
    );
    let scale = requested.base_size / 13.0;
    ResolvedChromeTypography {
        body: base.clone(),
        small: base.with_role(11.0 * scale, requested.regular_weight),
        control: base.with_role(13.0 * scale, requested.regular_weight),
        navigation: base.with_role(12.0 * scale, requested.emphasis_weight),
        caption: base.with_role(12.65 * scale, 400),
        heading: base.with_role(14.0 * scale, requested.heading_weight),
        shortcut: base.with_role(11.0 * scale, requested.regular_weight),
    }
}

fn resolve_terminal_typography(
    preferences: &AppearancePreferences,
    fonts: &AvailableFonts,
    diagnostics: &mut Vec<AppearanceDiagnostic>,
) -> ResolvedTerminalTypography {
    const DEFAULTS: [&str; 4] = [
        "JetBrainsMono Nerd Font",
        "JetBrainsMono Nerd Font Mono",
        "JetBrains Mono",
        "Menlo",
    ];
    let requested = &preferences.terminal.typography;
    let mut unavailable = false;
    let selected = match &requested.family {
        TerminalFontFamily::DefaultMonospace => DEFAULTS
            .iter()
            .find_map(|family| {
                fonts
                    .named(family)
                    .filter(|font| font.class == FontClass::Monospace)
            })
            .unwrap_or(&fonts.system_monospace),
        TerminalFontFamily::Named { family } => match fonts.named(family) {
            Some(font) if font.class == FontClass::Monospace => font,
            Some(_) => {
                diagnostics.push(AppearanceDiagnostic::TerminalFontNotMonospace);
                &fonts.system_monospace
            }
            None => {
                unavailable = true;
                &fonts.system_monospace
            }
        },
    };
    if unavailable {
        diagnostics.push(AppearanceDiagnostic::TerminalFontUnavailable);
    }
    let mut fallbacks = vec![String::from("Apple Color Emoji")];
    for family in DEFAULTS {
        if family != selected.family && !fallbacks.iter().any(|value| value == family) {
            fallbacks.push(family.to_owned());
        }
    }
    if fonts.system_monospace.family != selected.family
        && !fallbacks.contains(&fonts.system_monospace.family)
    {
        fallbacks.push(fonts.system_monospace.family.clone());
    }
    let mut regular = descriptor(
        selected,
        requested.base_size,
        requested.regular_weight,
        FontStyle::Normal,
        fallbacks,
    );
    regular.features = vec![
        String::from("-liga"),
        String::from("-clig"),
        String::from("-calt"),
    ];
    let bold = regular.with_style(requested.bold_weight, FontStyle::Normal);
    let italic_style = if requested.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    };
    ResolvedTerminalTypography {
        italic: regular.with_style(requested.regular_weight, italic_style),
        bold_italic: regular.with_style(requested.bold_weight, italic_style),
        bold,
        regular,
        cell_size: requested.base_size,
        line_height: requested.base_size * requested.line_height,
        render_italic: requested.italic,
    }
}

fn descriptor(
    font: &AvailableFont,
    size: f32,
    weight: u16,
    style: FontStyle,
    fallbacks: Vec<String>,
) -> ResolvedFontDescriptor {
    ResolvedFontDescriptor {
        primary_family: font.family.clone(),
        fallback_families: fallbacks,
        size,
        line_height: size,
        weight,
        style,
        features: Vec::new(),
        resolution_identity: font.resolution_identity.clone(),
    }
}

fn protocol_colors(colors: &TerminalColors) -> impl PartialEq + '_ {
    (
        &colors.foreground,
        &colors.background,
        &colors.normal,
        &colors.bright,
        &colors.dim,
        &colors.bright_foreground,
        &colors.dim_foreground,
        &colors.cursor,
    )
}

fn interaction_colors(colors: &TerminalColors) -> impl PartialEq + '_ {
    (
        &colors.cursor_text,
        &colors.selection_background,
        &colors.selection_foreground,
        &colors.find_match_background,
        &colors.find_match_foreground,
        &colors.find_active_match_background,
        &colors.find_active_match_foreground,
        &colors.hyperlink,
        &colors.visual_bell,
    )
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ResolutionError {
    #[error("invalid appearance preferences")]
    Preferences(#[source] PreferenceError),
    #[error("scheme kind does not match selection")]
    WrongSchemeKind,
    #[error("scheme appearance does not match selection")]
    AppearanceMismatch,
    #[error("resolved color has unsupported alpha")]
    UnsupportedAlpha,
}
