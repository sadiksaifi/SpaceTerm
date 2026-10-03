use std::sync::Arc;

use super::{
    Appearance, AppearancePreferences, ChromeColors, ChromeDensity, TerminalColors,
    TerminalFontFamily, ThemeCatalog, ThemeId, builtin, preferences::PreferenceError,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SystemAppearance {
    appearance: Option<Appearance>,
    composition: super::CompositionCapabilities,
}

impl Default for SystemAppearance {
    fn default() -> Self {
        Self::unavailable()
    }
}

impl SystemAppearance {
    #[cfg(test)]
    pub(crate) const fn available(appearance: Appearance) -> Self {
        Self {
            appearance: Some(appearance),
            composition: super::CompositionCapabilities::new(false, true),
        }
    }
    pub(crate) const fn unavailable() -> Self {
        Self {
            appearance: None,
            composition: super::CompositionCapabilities::new(false, true),
        }
    }
    pub(crate) const fn with_composition(
        mut self,
        composition: super::CompositionCapabilities,
    ) -> Self {
        self.composition = composition;
        self
    }
    pub(crate) const fn effective(self) -> Appearance {
        match self.appearance {
            Some(appearance) => appearance,
            None => Appearance::Dark,
        }
    }
}

impl From<Option<Appearance>> for SystemAppearance {
    fn from(value: Option<Appearance>) -> Self {
        Self {
            appearance: value,
            composition: super::CompositionCapabilities::new(false, true),
        }
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
    pub(crate) terminal_families: Vec<String>,
    pub(crate) emoji_family: String,
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
            terminal_families: vec![crate::bundled_font::FAMILY.into()],
            emoji_family: "emoji".into(),
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
    pub(crate) appearance: Appearance,
    pub(crate) colors: ChromeColors,
    pub(crate) composition: super::ResolvedWindowComposition,
    pub(crate) typography: ResolvedChromeTypography,
    pub(crate) density: ChromeDensity,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResolvedTerminalAppearance {
    pub(crate) requested_theme: ThemeId,
    pub(crate) effective_theme: ThemeId,
    pub(crate) appearance: Appearance,
    pub(crate) colors: TerminalColors,
    pub(crate) typography: ResolvedTerminalTypography,
    pub(crate) bold_as_bright: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AppearanceDiagnostic {
    SystemAppearanceUnavailable,
    TerminalThemeUnavailable { appearance: Appearance },
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
    pub(crate) native_appearance: bool,
    pub(crate) window_composition: bool,
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
            chrome_colors: previous.chrome.colors != next.chrome.colors,
            native_appearance: previous.chrome.appearance != next.chrome.appearance,
            window_composition: previous.chrome.composition != next.chrome.composition,
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

impl ThemeCatalog {
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
        if preferences.mode == super::AppearanceMode::Auto && system.appearance.is_none() {
            diagnostics.push(AppearanceDiagnostic::SystemAppearanceUnavailable);
        }
        let capabilities = system.composition;
        let system = system.effective();
        let appearance = preferences.mode.resolve(system);
        let requested_terminal = preferences.terminal.themes.get(appearance);
        let chrome_colors = super::compiler::compile_builtin_chrome(appearance);
        let composition = super::ResolvedWindowComposition::resolve(
            &preferences.window,
            capabilities,
            super::ChromeTone::of(chrome_colors.background),
        );
        let (effective_terminal, mut terminal_colors, found_terminal) =
            self.resolve_terminal_theme(requested_terminal, appearance)?;
        if !found_terminal {
            diagnostics.push(AppearanceDiagnostic::TerminalThemeUnavailable { appearance });
        }
        if found_terminal
            && let Some(overrides) = preferences.terminal.overrides.get(requested_terminal)
        {
            terminal_colors.apply(overrides);
        }
        terminal_colors
            .validate()
            .map_err(|_| ResolutionError::UnsupportedAlpha)?;
        let chrome_typography = resolve_chrome_typography(fonts);
        let terminal_typography = resolve_terminal_typography(preferences, fonts, &mut diagnostics);
        Ok(ResolvedAppearance {
            generation,
            chrome: Arc::new(ResolvedChromeAppearance {
                appearance,
                colors: chrome_colors,
                composition,
                typography: chrome_typography,
                density: preferences.window.density,
            }),
            terminal: Arc::new(ResolvedTerminalAppearance {
                requested_theme: requested_terminal.clone(),
                effective_theme: effective_terminal,
                appearance,
                colors: terminal_colors,
                typography: terminal_typography,
                bold_as_bright: preferences.terminal.rendering.bold_as_bright,
            }),
            diagnostics,
        })
    }

    fn resolve_terminal_theme(
        &self,
        requested: &ThemeId,
        appearance: Appearance,
    ) -> Result<(ThemeId, TerminalColors, bool), ResolutionError> {
        if let Some(theme) = self.get(requested) {
            if theme.appearance != appearance {
                return Err(ResolutionError::AppearanceMismatch);
            }
            let mut colors = builtin::terminal_base(appearance);
            colors.apply(&theme.colors);
            return Ok((requested.clone(), colors, true));
        }
        let id = builtin::fallback_id(appearance);
        Ok((id, builtin::terminal_base(appearance), false))
    }
}

fn resolve_chrome_typography(fonts: &AvailableFonts) -> ResolvedChromeTypography {
    const BASE_SIZE: f32 = 13.0;
    const REGULAR_WEIGHT: u16 = 400;
    const EMPHASIS_WEIGHT: u16 = 600;
    const HEADING_WEIGHT: u16 = 600;
    let base = descriptor(
        &fonts.system_ui,
        BASE_SIZE,
        REGULAR_WEIGHT,
        FontStyle::Normal,
        Vec::new(),
    );
    ResolvedChromeTypography {
        body: base.clone(),
        small: base.with_role(11.0, REGULAR_WEIGHT),
        control: base.with_role(13.0, REGULAR_WEIGHT),
        navigation: base.with_role(12.0, EMPHASIS_WEIGHT),
        caption: base.with_role(12.65, REGULAR_WEIGHT),
        heading: base.with_role(14.0, HEADING_WEIGHT),
        shortcut: base.with_role(11.0, REGULAR_WEIGHT),
    }
}

fn resolve_terminal_typography(
    preferences: &AppearancePreferences,
    fonts: &AvailableFonts,
    diagnostics: &mut Vec<AppearanceDiagnostic>,
) -> ResolvedTerminalTypography {
    let requested = &preferences.terminal.typography;
    let mut unavailable = false;
    let selected = match &requested.family {
        TerminalFontFamily::DefaultMonospace => fonts
            .terminal_families
            .iter()
            .map(String::as_str)
            .chain(std::iter::once(fonts.system_monospace.family.as_str()))
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
    let mut fallbacks = vec![fonts.emoji_family.clone()];
    for family in &fonts.terminal_families {
        if family != &selected.family && !fallbacks.iter().any(|value| value == family) {
            fallbacks.push(family.clone());
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
    #[error("theme appearance does not match selection")]
    AppearanceMismatch,
    #[error("resolved color has unsupported alpha")]
    UnsupportedAlpha,
}
