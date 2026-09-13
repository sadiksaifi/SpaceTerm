//! The identity, order, and search vocabulary of every Settings Row.
//!
//! One table owns row identity so navigation, search, rendering, and reset all agree. A row that
//! is not listed here cannot be found by Settings Search, so the suite asserts that every
//! [`SettingsRowId`] appears exactly once.

use crate::appearance::ResetTarget;

/// One named group of Settings presented as one navigation entry and one content region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsSectionId {
    /// What both surfaces share: the one appearance mode, and the scheme each surface wears in it.
    Appearance,
    /// SpaceTerm's own typography.
    Interface,
    /// Terminal typography and text rendering.
    Terminal,
    /// The scheme library both surfaces draw from.
    ColorSchemes,
}

impl SettingsSectionId {
    /// Every section in presentation order.
    pub(super) const ALL: [Self; 4] = [
        Self::Appearance,
        Self::Interface,
        Self::Terminal,
        Self::ColorSchemes,
    ];

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Interface => "Interface",
            Self::Terminal => "Terminal",
            Self::ColorSchemes => "Color Schemes",
        }
    }

    /// The shorter form the navigation list presents.
    pub(super) const fn navigation_title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Interface => "Interface",
            Self::Terminal => "Terminal",
            Self::ColorSchemes => "Color Schemes",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Appearance => {
                "One light or dark setting for the whole application. Each surface still wears its \
                 own color scheme."
            }
            Self::Interface => {
                "Type in SpaceTerm's own windows, tabs, and panels. Terminal output is unaffected."
            }
            Self::Terminal => "Type and text rendering in terminal output.",
            Self::ColorSchemes => {
                "Every scheme installed for the interface and the terminal. Built-in schemes are \
                 always available; imported schemes can be removed."
            }
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Appearance => "settings-section-appearance",
            Self::Interface => "settings-section-interface",
            Self::Terminal => "settings-section-terminal",
            Self::ColorSchemes => "settings-section-color-schemes",
        }
    }
}

/// One labeled Setting control within a Section.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsRowId {
    /// The one light, dark, or automatic choice, which both surfaces follow.
    AppearanceMode,
    ChromeDensity,
    ChromeScheme,
    ChromeLightScheme,
    ChromeDarkScheme,
    TerminalScheme,
    TerminalLightScheme,
    TerminalDarkScheme,
    ChromeFontFamily,
    ChromeBaseSize,
    ChromeRegularWeight,
    ChromeEmphasisWeight,
    ChromeHeadingWeight,
    TerminalFontFamily,
    TerminalBaseSize,
    TerminalLineHeight,
    TerminalRegularWeight,
    TerminalBoldWeight,
    TerminalItalic,
    TerminalBoldAsBright,
    InterfaceSchemes,
    TerminalSchemes,
    SchemeInterchange,
}

impl SettingsRowId {
    pub(super) fn descriptor(self) -> &'static SettingsRowDescriptor {
        ROWS.iter()
            .find(|descriptor| descriptor.id == self)
            .expect("every row identity is listed in the catalog")
    }

    /// The reset target restoring this row alone, when the row holds a resettable preference.
    ///
    /// A scheme row restores its own scheme and leaves the appearance mode alone, because the mode
    /// belongs to the one control that spans both surfaces.
    pub(super) fn reset_target(self) -> Option<ResetTarget> {
        Some(match self {
            Self::AppearanceMode => ResetTarget::SchemeSelections,
            Self::ChromeDensity => ResetTarget::ChromeDensity,
            Self::ChromeScheme | Self::ChromeLightScheme | Self::ChromeDarkScheme => {
                ResetTarget::ChromeSchemeChoice
            }
            Self::TerminalScheme | Self::TerminalLightScheme | Self::TerminalDarkScheme => {
                ResetTarget::TerminalSchemeChoice
            }
            Self::ChromeFontFamily => ResetTarget::ChromeFontFamily,
            Self::ChromeBaseSize => ResetTarget::ChromeBaseSize,
            Self::ChromeRegularWeight => ResetTarget::ChromeRegularWeight,
            Self::ChromeEmphasisWeight => ResetTarget::ChromeEmphasisWeight,
            Self::ChromeHeadingWeight => ResetTarget::ChromeHeadingWeight,
            Self::TerminalFontFamily => ResetTarget::TerminalFontFamily,
            Self::TerminalBaseSize => ResetTarget::TerminalBaseSize,
            Self::TerminalRegularWeight => ResetTarget::TerminalRegularWeight,
            Self::TerminalBoldWeight => ResetTarget::TerminalBoldWeight,
            Self::TerminalLineHeight => ResetTarget::TerminalLineHeight,
            Self::TerminalItalic => ResetTarget::TerminalItalic,
            Self::TerminalBoldAsBright => ResetTarget::TerminalBoldAsBright,
            Self::InterfaceSchemes | Self::TerminalSchemes | Self::SchemeInterchange => {
                return None;
            }
        })
    }
}

/// A row's presentation and search vocabulary.
pub(super) struct SettingsRowDescriptor {
    pub(super) id: SettingsRowId,
    pub(super) section: SettingsSectionId,
    /// The titled box this row shares with the rows next to it in the table.
    ///
    /// Rows carrying the same group title in one section render as one box, so the order here is
    /// also the grouping: a row that leaves its neighbours starts a new box.
    pub(super) group: &'static str,
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
            || self.group.to_ascii_lowercase().contains(query)
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
        id: SettingsRowId::AppearanceMode,
        section: SettingsSectionId::Appearance,
        group: "Mode",
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
        selector: "settings-row-appearance-mode",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeDensity,
        section: SettingsSectionId::Appearance,
        group: "Mode",
        label: "Density",
        keywords: &["compact", "comfortable", "spacing", "padding"],
        selector: "settings-row-chrome-density",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Interface",
        keywords: &["colour", "palette", "theme", "scheme", "chrome"],
        selector: "settings-row-chrome-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeLightScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Interface light",
        keywords: &["colour", "palette", "theme", "light", "chrome"],
        selector: "settings-row-chrome-light-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeDarkScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Interface dark",
        keywords: &["colour", "palette", "theme", "dark", "chrome"],
        selector: "settings-row-chrome-dark-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Terminal",
        keywords: &["colour", "palette", "theme", "scheme", "ansi"],
        selector: "settings-row-terminal-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalLightScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Terminal light",
        keywords: &["colour", "palette", "theme", "light", "ansi"],
        selector: "settings-row-terminal-light-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalDarkScheme,
        section: SettingsSectionId::Appearance,
        group: "Color scheme",
        label: "Terminal dark",
        keywords: &["colour", "palette", "theme", "dark", "ansi"],
        selector: "settings-row-terminal-dark-scheme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeFontFamily,
        section: SettingsSectionId::Interface,
        group: "Font",
        label: "Family",
        keywords: &["typeface", "family", "font", "interface"],
        selector: "settings-row-chrome-font-family",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeBaseSize,
        section: SettingsSectionId::Interface,
        group: "Font",
        label: "Size",
        keywords: &["points", "size", "bigger", "smaller", "zoom", "font"],
        selector: "settings-row-chrome-base-size",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeRegularWeight,
        section: SettingsSectionId::Interface,
        group: "Weight",
        label: "Regular",
        keywords: &["bold", "weight", "font"],
        selector: "settings-row-chrome-regular-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeEmphasisWeight,
        section: SettingsSectionId::Interface,
        group: "Weight",
        label: "Emphasis",
        keywords: &["bold", "weight", "semibold", "font"],
        selector: "settings-row-chrome-emphasis-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ChromeHeadingWeight,
        section: SettingsSectionId::Interface,
        group: "Weight",
        label: "Heading",
        keywords: &["bold", "weight", "title", "font"],
        selector: "settings-row-chrome-heading-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalFontFamily,
        section: SettingsSectionId::Terminal,
        group: "Font",
        label: "Family",
        keywords: &["typeface", "family", "monospace", "font"],
        selector: "settings-row-terminal-font-family",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBaseSize,
        section: SettingsSectionId::Terminal,
        group: "Font",
        label: "Size",
        keywords: &["points", "size", "bigger", "smaller", "zoom", "font"],
        selector: "settings-row-terminal-base-size",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalLineHeight,
        section: SettingsSectionId::Terminal,
        group: "Font",
        label: "Line height",
        keywords: &["leading", "spacing", "line"],
        selector: "settings-row-terminal-line-height",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalRegularWeight,
        section: SettingsSectionId::Terminal,
        group: "Weight",
        label: "Regular",
        keywords: &["weight", "font"],
        selector: "settings-row-terminal-regular-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldWeight,
        section: SettingsSectionId::Terminal,
        group: "Weight",
        label: "Bold",
        keywords: &["weight", "font", "bold"],
        selector: "settings-row-terminal-bold-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalItalic,
        section: SettingsSectionId::Terminal,
        group: "Rendering",
        label: "Italic text",
        keywords: &["oblique", "slant", "italic"],
        selector: "settings-row-terminal-italic",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldAsBright,
        section: SettingsSectionId::Terminal,
        group: "Rendering",
        label: "Show bold text in bright colors",
        keywords: &["ansi", "bright", "bold", "colour"],
        selector: "settings-row-terminal-bold-as-bright",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::InterfaceSchemes,
        section: SettingsSectionId::ColorSchemes,
        group: "Interface",
        label: "Interface schemes",
        keywords: &[
            "builtin",
            "custom",
            "remove",
            "delete",
            "list",
            "unavailable",
            "fallback",
            "missing",
            "chrome",
        ],
        selector: "settings-row-interface-schemes",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalSchemes,
        section: SettingsSectionId::ColorSchemes,
        group: "Terminal",
        label: "Terminal schemes",
        keywords: &[
            "builtin",
            "custom",
            "remove",
            "delete",
            "list",
            "unavailable",
            "fallback",
            "missing",
            "ansi",
        ],
        selector: "settings-row-terminal-schemes",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::SchemeInterchange,
        section: SettingsSectionId::ColorSchemes,
        group: "Import and export",
        label: "Import and export",
        keywords: &["zed", "import", "export", "file", "package", "share"],
        selector: "settings-row-scheme-interchange",
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The complete row identity set, so the catalog cannot silently omit one.
    const EVERY_ROW: [SettingsRowId; 23] = [
        SettingsRowId::AppearanceMode,
        SettingsRowId::ChromeDensity,
        SettingsRowId::ChromeScheme,
        SettingsRowId::ChromeLightScheme,
        SettingsRowId::ChromeDarkScheme,
        SettingsRowId::TerminalScheme,
        SettingsRowId::TerminalLightScheme,
        SettingsRowId::TerminalDarkScheme,
        SettingsRowId::ChromeFontFamily,
        SettingsRowId::ChromeBaseSize,
        SettingsRowId::ChromeRegularWeight,
        SettingsRowId::ChromeEmphasisWeight,
        SettingsRowId::ChromeHeadingWeight,
        SettingsRowId::TerminalFontFamily,
        SettingsRowId::TerminalBaseSize,
        SettingsRowId::TerminalLineHeight,
        SettingsRowId::TerminalRegularWeight,
        SettingsRowId::TerminalBoldWeight,
        SettingsRowId::TerminalItalic,
        SettingsRowId::TerminalBoldAsBright,
        SettingsRowId::InterfaceSchemes,
        SettingsRowId::TerminalSchemes,
        SettingsRowId::SchemeInterchange,
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
    fn every_section_presents_at_least_one_row() {
        for section in SettingsSectionId::ALL {
            assert!(
                ROWS.iter().any(|row| row.section == section),
                "{section:?} has no rows"
            );
        }
    }

    /// Rendering makes one box per run of neighbouring rows sharing a group, so a group that
    /// appears twice in one section would silently become two identical boxes.
    #[test]
    fn every_group_occupies_one_run_of_the_table() {
        let mut seen = HashSet::new();
        let mut previous: Option<(SettingsSectionId, &str)> = None;
        for row in ROWS {
            let current = (row.section, row.group);
            if previous == Some(current) {
                continue;
            }
            assert!(
                seen.insert(current),
                "{current:?} is split across the table"
            );
            previous = Some(current);
        }
    }

    /// One appearance mode governs the whole application, so the catalog offers exactly one.
    #[test]
    fn one_row_owns_the_appearance_mode() {
        let modes = ROWS
            .iter()
            .filter(|row| row.id.reset_target() == Some(ResetTarget::SchemeSelections))
            .count();

        assert_eq!(modes, 1);
    }

    /// A scheme row changes a scheme, so its reset leaves the shared appearance mode alone.
    #[test]
    fn a_scheme_row_resets_only_its_own_scheme() {
        for (row, expected) in [
            (SettingsRowId::ChromeScheme, ResetTarget::ChromeSchemeChoice),
            (
                SettingsRowId::ChromeLightScheme,
                ResetTarget::ChromeSchemeChoice,
            ),
            (
                SettingsRowId::TerminalDarkScheme,
                ResetTarget::TerminalSchemeChoice,
            ),
        ] {
            assert_eq!(row.reset_target(), Some(expected));
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
        assert!(matching_rows("automatic").contains(&SettingsRowId::AppearanceMode));
    }

    /// The diagnostics readout is part of the scheme library rather than a row of its own, so the
    /// words a person searches for when a scheme is missing still reach that page.
    #[test]
    fn a_missing_scheme_query_reaches_the_library() {
        assert!(matching_rows("unavailable").contains(&SettingsRowId::InterfaceSchemes));
        assert!(matching_rows("fallback").contains(&SettingsRowId::TerminalSchemes));
    }

    #[test]
    fn a_section_query_matches_that_sections_rows() {
        let matches = matching_rows("terminal");

        assert!(matches.contains(&SettingsRowId::TerminalItalic));
        assert!(!matches.contains(&SettingsRowId::ChromeDensity));
    }

    /// A group title is a heading a person can read on the page, so searching it reaches its rows.
    #[test]
    fn a_group_query_matches_that_groups_rows() {
        let matches = matching_rows("rendering");

        assert!(matches.contains(&SettingsRowId::TerminalBoldAsBright));
        assert!(!matches.contains(&SettingsRowId::TerminalBaseSize));
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
                SettingsRowId::InterfaceSchemes
                    | SettingsRowId::TerminalSchemes
                    | SettingsRowId::SchemeInterchange
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
