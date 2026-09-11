//! The identity, order, and search vocabulary of every Settings Row.
//!
//! One table owns row identity so navigation, search, rendering, and reset all agree. A row that
//! is not listed here cannot be found by Settings Search, so the suite asserts that every
//! [`SettingsRowId`] appears exactly once.

use crate::appearance::ResetTarget;

/// One named group of Settings presented as one navigation entry and one content region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsSectionId {
    Appearance,
    Terminal,
    ColorSchemes,
}

impl SettingsSectionId {
    /// Every section in presentation order. The order is also the scroll order.
    pub(super) const ALL: [Self; 3] = [Self::Appearance, Self::Terminal, Self::ColorSchemes];

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Application Appearance",
            Self::Terminal => "Terminal Appearance",
            Self::ColorSchemes => "Color Schemes",
        }
    }

    /// The shorter form the navigation list presents.
    pub(super) const fn navigation_title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Terminal => "Terminal",
            Self::ColorSchemes => "Color Schemes",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Appearance => {
                "How SpaceTerm's own windows, tabs, and panels look, independently of the terminal."
            }
            Self::Terminal => {
                "How terminal output looks. These choices do not affect SpaceTerm's own chrome."
            }
            Self::ColorSchemes => {
                "The schemes available to both sections above, and where they come from."
            }
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Appearance => "settings-section-appearance",
            Self::Terminal => "settings-section-terminal",
            Self::ColorSchemes => "settings-section-color-schemes",
        }
    }
}

/// One labeled Setting control within a Section.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsRowId {
    ChromeAppearanceMode,
    ChromeScheme,
    ChromeLightScheme,
    ChromeDarkScheme,
    ChromeDensity,
    ChromeFontFamily,
    ChromeBaseSize,
    ChromeRegularWeight,
    ChromeEmphasisWeight,
    ChromeHeadingWeight,
    TerminalAppearanceMode,
    TerminalScheme,
    TerminalLightScheme,
    TerminalDarkScheme,
    TerminalFontFamily,
    TerminalBaseSize,
    TerminalRegularWeight,
    TerminalBoldWeight,
    TerminalLineHeight,
    TerminalItalic,
    TerminalBoldAsBright,
    InstalledSchemes,
    SchemeInterchange,
    AppearanceDiagnostics,
}

impl SettingsRowId {
    pub(super) fn descriptor(self) -> &'static SettingsRowDescriptor {
        ROWS.iter()
            .find(|descriptor| descriptor.id == self)
            .expect("every row identity is listed in the catalog")
    }

    /// The reset target restoring this row alone, when the row holds a resettable preference.
    pub(super) fn reset_target(self) -> Option<ResetTarget> {
        Some(match self {
            Self::ChromeAppearanceMode | Self::ChromeScheme => ResetTarget::ChromeSchemeSelection,
            Self::ChromeLightScheme | Self::ChromeDarkScheme => ResetTarget::ChromeSchemeSelection,
            Self::ChromeDensity => ResetTarget::ChromeDensity,
            Self::ChromeFontFamily => ResetTarget::ChromeFontFamily,
            Self::ChromeBaseSize => ResetTarget::ChromeBaseSize,
            Self::ChromeRegularWeight => ResetTarget::ChromeRegularWeight,
            Self::ChromeEmphasisWeight => ResetTarget::ChromeEmphasisWeight,
            Self::ChromeHeadingWeight => ResetTarget::ChromeHeadingWeight,
            Self::TerminalAppearanceMode | Self::TerminalScheme => {
                ResetTarget::TerminalSchemeSelection
            }
            Self::TerminalLightScheme | Self::TerminalDarkScheme => {
                ResetTarget::TerminalSchemeSelection
            }
            Self::TerminalFontFamily => ResetTarget::TerminalFontFamily,
            Self::TerminalBaseSize => ResetTarget::TerminalBaseSize,
            Self::TerminalRegularWeight => ResetTarget::TerminalRegularWeight,
            Self::TerminalBoldWeight => ResetTarget::TerminalBoldWeight,
            Self::TerminalLineHeight => ResetTarget::TerminalLineHeight,
            Self::TerminalItalic => ResetTarget::TerminalItalic,
            Self::TerminalBoldAsBright => ResetTarget::TerminalBoldAsBright,
            Self::InstalledSchemes | Self::SchemeInterchange | Self::AppearanceDiagnostics => {
                return None;
            }
        })
    }
}

/// A row's presentation and search vocabulary.
pub(super) struct SettingsRowDescriptor {
    pub(super) id: SettingsRowId,
    pub(super) section: SettingsSectionId,
    pub(super) label: &'static str,
    /// Words a person might search for that do not appear in the label.
    pub(super) keywords: &'static [&'static str],
    pub(super) selector: &'static str,
}

impl SettingsRowDescriptor {
    /// Whether this row answers `query`, which the caller has already lowercased and trimmed.
    fn matches(&self, query: &str) -> bool {
        self.label.to_ascii_lowercase().contains(query)
            || self.section.title().to_ascii_lowercase().contains(query)
            || self
                .keywords
                .iter()
                .any(|keyword| keyword.contains(query) || query.contains(keyword))
    }
}

/// Returns the rows answering `query`, or every row when the query is empty.
pub(super) fn matching_rows(query: &str) -> Vec<SettingsRowId> {
    let query = query.trim().to_ascii_lowercase();
    ROWS.iter()
        .filter(|descriptor| query.is_empty() || descriptor.matches(&query))
        .map(|descriptor| descriptor.id)
        .collect()
}

pub(super) const ROWS: &[SettingsRowDescriptor] = &[
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeAppearanceMode,
        section: SettingsSectionId::Appearance,
        label: "Appearance",
        keywords: &[
            "light",
            "dark",
            "auto",
            "automatic",
            "system",
            "mode",
            "theme",
        ],
        selector: "settings-row-chrome-appearance-mode",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeScheme,
        section: SettingsSectionId::Appearance,
        label: "Color scheme",
        keywords: &["colour", "palette", "theme", "scheme"],
        selector: "settings-row-chrome-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeLightScheme,
        section: SettingsSectionId::Appearance,
        label: "Light scheme",
        keywords: &["colour", "palette", "theme", "light"],
        selector: "settings-row-chrome-light-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeDarkScheme,
        section: SettingsSectionId::Appearance,
        label: "Dark scheme",
        keywords: &["colour", "palette", "theme", "dark"],
        selector: "settings-row-chrome-dark-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeDensity,
        section: SettingsSectionId::Appearance,
        label: "Density",
        keywords: &["compact", "comfortable", "spacing", "padding"],
        selector: "settings-row-chrome-density",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeFontFamily,
        section: SettingsSectionId::Appearance,
        label: "Interface font",
        keywords: &["typeface", "family", "font"],
        selector: "settings-row-chrome-font-family",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeBaseSize,
        section: SettingsSectionId::Appearance,
        label: "Interface font size",
        keywords: &["points", "size", "bigger", "smaller", "zoom"],
        selector: "settings-row-chrome-base-size",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeRegularWeight,
        section: SettingsSectionId::Appearance,
        label: "Regular weight",
        keywords: &["bold", "weight", "font"],
        selector: "settings-row-chrome-regular-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeEmphasisWeight,
        section: SettingsSectionId::Appearance,
        label: "Emphasis weight",
        keywords: &["bold", "weight", "semibold", "font"],
        selector: "settings-row-chrome-emphasis-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeHeadingWeight,
        section: SettingsSectionId::Appearance,
        label: "Heading weight",
        keywords: &["bold", "weight", "title", "font"],
        selector: "settings-row-chrome-heading-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalAppearanceMode,
        section: SettingsSectionId::Terminal,
        label: "Appearance",
        keywords: &[
            "light",
            "dark",
            "auto",
            "automatic",
            "system",
            "mode",
            "theme",
        ],
        selector: "settings-row-terminal-appearance-mode",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalScheme,
        section: SettingsSectionId::Terminal,
        label: "Color scheme",
        keywords: &["colour", "palette", "theme", "scheme", "ansi"],
        selector: "settings-row-terminal-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalLightScheme,
        section: SettingsSectionId::Terminal,
        label: "Light scheme",
        keywords: &["colour", "palette", "theme", "light"],
        selector: "settings-row-terminal-light-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalDarkScheme,
        section: SettingsSectionId::Terminal,
        label: "Dark scheme",
        keywords: &["colour", "palette", "theme", "dark"],
        selector: "settings-row-terminal-dark-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalFontFamily,
        section: SettingsSectionId::Terminal,
        label: "Terminal font",
        keywords: &["typeface", "family", "monospace", "font"],
        selector: "settings-row-terminal-font-family",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBaseSize,
        section: SettingsSectionId::Terminal,
        label: "Terminal font size",
        keywords: &["points", "size", "bigger", "smaller", "zoom"],
        selector: "settings-row-terminal-base-size",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalRegularWeight,
        section: SettingsSectionId::Terminal,
        label: "Regular weight",
        keywords: &["weight", "font"],
        selector: "settings-row-terminal-regular-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldWeight,
        section: SettingsSectionId::Terminal,
        label: "Bold weight",
        keywords: &["weight", "font", "bold"],
        selector: "settings-row-terminal-bold-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalLineHeight,
        section: SettingsSectionId::Terminal,
        label: "Line height",
        keywords: &["leading", "spacing", "line"],
        selector: "settings-row-terminal-line-height",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalItalic,
        section: SettingsSectionId::Terminal,
        label: "Italic text",
        keywords: &["oblique", "slant", "italic"],
        selector: "settings-row-terminal-italic",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldAsBright,
        section: SettingsSectionId::Terminal,
        label: "Show bold text in bright colors",
        keywords: &["ansi", "bright", "bold", "colour"],
        selector: "settings-row-terminal-bold-as-bright",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::InstalledSchemes,
        section: SettingsSectionId::ColorSchemes,
        label: "Installed schemes",
        keywords: &["builtin", "custom", "remove", "delete", "list"],
        selector: "settings-row-installed-schemes",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::SchemeInterchange,
        section: SettingsSectionId::ColorSchemes,
        label: "Import and export",
        keywords: &["zed", "import", "export", "file", "package", "share"],
        selector: "settings-row-scheme-interchange",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::AppearanceDiagnostics,
        section: SettingsSectionId::ColorSchemes,
        label: "Diagnostics",
        keywords: &["unavailable", "fallback", "missing", "warning"],
        selector: "settings-row-appearance-diagnostics",
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The complete row identity set, so the catalog cannot silently omit one.
    const EVERY_ROW: [SettingsRowId; 24] = [
        SettingsRowId::ChromeAppearanceMode,
        SettingsRowId::ChromeScheme,
        SettingsRowId::ChromeLightScheme,
        SettingsRowId::ChromeDarkScheme,
        SettingsRowId::ChromeDensity,
        SettingsRowId::ChromeFontFamily,
        SettingsRowId::ChromeBaseSize,
        SettingsRowId::ChromeRegularWeight,
        SettingsRowId::ChromeEmphasisWeight,
        SettingsRowId::ChromeHeadingWeight,
        SettingsRowId::TerminalAppearanceMode,
        SettingsRowId::TerminalScheme,
        SettingsRowId::TerminalLightScheme,
        SettingsRowId::TerminalDarkScheme,
        SettingsRowId::TerminalFontFamily,
        SettingsRowId::TerminalBaseSize,
        SettingsRowId::TerminalRegularWeight,
        SettingsRowId::TerminalBoldWeight,
        SettingsRowId::TerminalLineHeight,
        SettingsRowId::TerminalItalic,
        SettingsRowId::TerminalBoldAsBright,
        SettingsRowId::InstalledSchemes,
        SettingsRowId::SchemeInterchange,
        SettingsRowId::AppearanceDiagnostics,
    ];

    #[test]
    fn every_row_identity_is_listed_exactly_once() {
        for id in EVERY_ROW {
            let listed = ROWS.iter().filter(|row| row.id == id).count();
            assert_eq!(listed, 1, "{id:?} should be listed exactly once");
        }
        assert_eq!(ROWS.len(), EVERY_ROW.len());
    }

    #[test]
    fn every_row_selector_is_unique() {
        let selectors = ROWS.iter().map(|row| row.selector).collect::<HashSet<_>>();

        assert_eq!(selectors.len(), ROWS.len());
    }

    #[test]
    fn every_row_belongs_to_a_presented_section() {
        for row in ROWS {
            assert!(SettingsSectionId::ALL.contains(&row.section));
        }
    }

    #[test]
    fn an_empty_query_matches_every_row() {
        assert_eq!(matching_rows("   ").len(), ROWS.len());
    }

    #[test]
    fn a_label_query_matches_only_the_relevant_rows() {
        assert_eq!(
            matching_rows("line height"),
            vec![SettingsRowId::TerminalLineHeight]
        );
    }

    #[test]
    fn a_keyword_query_reaches_a_row_whose_label_omits_the_word() {
        assert!(matching_rows("leading").contains(&SettingsRowId::TerminalLineHeight));
        assert!(matching_rows("zed").contains(&SettingsRowId::SchemeInterchange));
        assert!(matching_rows("automatic").contains(&SettingsRowId::ChromeAppearanceMode));
    }

    #[test]
    fn a_section_query_matches_that_sections_rows() {
        let matches = matching_rows("terminal appearance");

        assert!(matches.contains(&SettingsRowId::TerminalItalic));
        assert!(!matches.contains(&SettingsRowId::ChromeDensity));
    }

    #[test]
    fn an_unmatched_query_matches_nothing() {
        assert!(matching_rows("kubernetes").is_empty());
    }

    #[test]
    fn every_preference_row_maps_to_a_reset_target() {
        for row in ROWS {
            let resettable = !matches!(
                row.id,
                SettingsRowId::InstalledSchemes
                    | SettingsRowId::SchemeInterchange
                    | SettingsRowId::AppearanceDiagnostics
            );
            assert_eq!(
                row.id.reset_target().is_some(),
                resettable,
                "{:?} reset mapping disagrees with its kind",
                row.id
            );
        }
    }

    #[test]
    fn a_descriptor_is_reachable_from_its_identity() {
        assert_eq!(
            SettingsRowId::TerminalLineHeight.descriptor().label,
            "Line height"
        );
    }
}
