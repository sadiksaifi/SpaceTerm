//! The identity, order, and search vocabulary of every Settings Row.
//!
//! One table owns row identity so navigation, search, rendering, and reset all agree. A row that
//! is not listed here cannot be found by Settings Search, so the suite asserts that every
//! [`SettingsRowId`] appears exactly once.

use spaceterm_ui::{FuzzyTarget, fuzzy_filter};

use crate::appearance::{Appearance, ResetTarget};

/// One named group of Settings presented as one navigation entry and one content region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsSectionId {
    /// How SpaceTerm's windows present themselves: density, transparency, and blur.
    Interface,
    /// Terminal typography and text rendering.
    Font,
    /// The light, dark, or automatic appearance, and the Terminal Theme each appearance uses.
    Themes,
    /// System permissions that tools running in SpaceTerm rely on.
    Privacy,
}

impl SettingsSectionId {
    /// Every section in presentation order.
    pub(super) const ALL: [Self; 4] = [Self::Interface, Self::Font, Self::Themes, Self::Privacy];

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Interface => "Interface",
            Self::Font => "Font",
            Self::Themes => "Themes",
            Self::Privacy => "Privacy",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Interface => "Density, transparency, and blur for SpaceTerm's windows.",
            Self::Font => "The typeface and text rendering in terminal panes.",
            Self::Themes => {
                "Light, dark, or automatic appearance, and the colors terminal panes use in each. Themes published for Zed work too."
            }
            Self::Privacy => {
                "System permissions that voice and other tools running in SpaceTerm rely on."
            }
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Interface => "settings-section-interface",
            Self::Font => "settings-section-font",
            Self::Themes => "settings-section-themes",
            Self::Privacy => "settings-section-privacy",
        }
    }
}

/// One labeled Setting control within a Section.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) enum SettingsRowId {
    /// The shared light, dark, or automatic choice.
    AppearanceMode,
    Transparency,
    Blur,
    Density,
    /// The Terminal Theme in use, or both slots under Auto.
    TerminalTheme,
    TerminalFontFamily,
    TerminalBaseSize,
    TerminalLineHeight,
    TerminalRegularWeight,
    TerminalBoldWeight,
    TerminalItalic,
    TerminalBoldAsBright,
    /// The installed Terminal Themes for the chosen appearance, and the way to get more.
    InstalledThemes,
    /// The system's microphone authorization, which voice tools in a Terminal Session inherit.
    MicrophoneAccess,
}

impl SettingsRowId {
    pub(super) fn descriptor(self) -> &'static SettingsRowDescriptor {
        ROWS.iter()
            .find(|descriptor| descriptor.id == self)
            .expect("every row identity is listed in the catalog")
    }

    /// The reset target restoring this row alone, when the row holds a resettable preference.
    ///
    /// A theme row restores its own theme and leaves the appearance mode alone, because the mode
    /// belongs to the shared Appearance control.
    pub(super) fn reset_target(self, appearance: Appearance) -> Option<ResetTarget> {
        Some(match self {
            Self::AppearanceMode => ResetTarget::AppearanceMode,
            Self::Transparency => ResetTarget::Transparency,
            Self::Blur => ResetTarget::Blur,
            Self::Density => ResetTarget::Density,
            Self::TerminalTheme => ResetTarget::TerminalTheme(appearance),
            Self::TerminalFontFamily => ResetTarget::TerminalFontFamily,
            Self::TerminalBaseSize => ResetTarget::TerminalBaseSize,
            Self::TerminalRegularWeight => ResetTarget::TerminalRegularWeight,
            Self::TerminalBoldWeight => ResetTarget::TerminalBoldWeight,
            Self::TerminalLineHeight => ResetTarget::TerminalLineHeight,
            Self::TerminalItalic => ResetTarget::TerminalItalic,
            Self::TerminalBoldAsBright => ResetTarget::TerminalBoldAsBright,
            Self::InstalledThemes | Self::MicrophoneAccess => {
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SettingsRowMatch {
    pub(super) id: SettingsRowId,
    pub(super) score: i64,
    pub(super) matched_indices: Vec<usize>,
}

pub(super) fn matching_row_matches(query: &str) -> Vec<SettingsRowMatch> {
    let fields = ROWS
        .iter()
        .enumerate()
        .flat_map(|(row_index, descriptor)| {
            std::iter::once((row_index, true, descriptor.label)).chain(
                descriptor
                    .keywords
                    .iter()
                    .map(move |keyword| (row_index, false, *keyword)),
            )
        })
        .collect::<Vec<_>>();
    let mut seen_rows = vec![false; ROWS.len()];

    fuzzy_filter(&fields, query, |(_, _, text)| FuzzyTarget::new(text))
        .into_iter()
        .filter_map(|matched| {
            let (row_index, is_label, _) = fields[matched.item_index()];
            if std::mem::replace(&mut seen_rows[row_index], true) {
                return None;
            }
            Some(SettingsRowMatch {
                id: ROWS[row_index].id,
                score: matched.score(),
                matched_indices: if is_label {
                    matched.field_highlight_indices(0)
                } else {
                    Vec::new()
                },
            })
        })
        .collect()
}

/// Returns the rows answering `query`, or every row when the query is empty.
pub(super) fn matching_rows(query: &str) -> Vec<SettingsRowId> {
    matching_row_matches(query)
        .into_iter()
        .map(|matched| matched.id)
        .collect()
}

pub(super) const ROWS: &[SettingsRowDescriptor] = &[
    SettingsRowDescriptor {
        id: SettingsRowId::Density,
        section: SettingsSectionId::Interface,
        group: "Window",
        label: "Density",
        keywords: &["compact", "comfortable", "spacing", "padding"],
        selector: "settings-row-density",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::Transparency,
        section: SettingsSectionId::Interface,
        group: "Window",
        label: "Transparency",
        keywords: &["opacity", "transparent", "opaque", "window", "terminal"],
        selector: "settings-row-transparency",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::Blur,
        section: SettingsSectionId::Interface,
        group: "Window",
        label: "Blur",
        keywords: &["blurred", "background", "window", "glass"],
        selector: "settings-row-blur",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalFontFamily,
        section: SettingsSectionId::Font,
        group: "Typeface",
        label: "Family",
        keywords: &["typeface", "family", "monospace", "font"],
        selector: "settings-row-terminal-font-family",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBaseSize,
        section: SettingsSectionId::Font,
        group: "Typeface",
        label: "Size",
        keywords: &["points", "size", "bigger", "smaller", "zoom", "font"],
        selector: "settings-row-terminal-base-size",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalLineHeight,
        section: SettingsSectionId::Font,
        group: "Typeface",
        label: "Line height",
        keywords: &["leading", "spacing", "line"],
        selector: "settings-row-terminal-line-height",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalRegularWeight,
        section: SettingsSectionId::Font,
        group: "Weight",
        label: "Regular",
        keywords: &["weight", "font"],
        selector: "settings-row-terminal-regular-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldWeight,
        section: SettingsSectionId::Font,
        group: "Weight",
        label: "Bold",
        keywords: &["weight", "font", "bold"],
        selector: "settings-row-terminal-bold-weight",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalItalic,
        section: SettingsSectionId::Font,
        group: "Rendering",
        label: "Italic text",
        keywords: &["oblique", "slant", "italic"],
        selector: "settings-row-terminal-italic",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalBoldAsBright,
        section: SettingsSectionId::Font,
        group: "Rendering",
        label: "Show bold text in bright colors",
        keywords: &["ansi", "bright", "bold", "colour"],
        selector: "settings-row-terminal-bold-as-bright",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::AppearanceMode,
        section: SettingsSectionId::Themes,
        // The appearance and the theme it shows lead the page together and need no title: they
        // are what the page is about.
        group: "",
        label: "Appearance",
        keywords: &[
            "light",
            "dark",
            "auto",
            "automatic",
            "system",
            "mode",
            "interface",
            "chrome",
            "terminal",
        ],
        selector: "settings-row-appearance-mode",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::TerminalTheme,
        section: SettingsSectionId::Themes,
        group: "",
        label: "Current theme",
        keywords: &[
            "color",
            "colour",
            "palette",
            "terminal theme",
            "ansi",
            "preview",
            "light",
            "dark",
            "unavailable",
            "fallback",
            "missing",
        ],
        selector: "settings-row-terminal-theme",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::InstalledThemes,
        section: SettingsSectionId::Themes,
        group: "Themes",
        label: "Installed themes",
        keywords: &[
            "choose",
            "gallery",
            "remove",
            "delete",
            "get more",
            "download",
            "install",
            "update",
            "zed",
            "extension",
            "registry",
            "import",
            "file",
            "json",
        ],
        selector: "settings-row-installed-themes",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::MicrophoneAccess,
        section: SettingsSectionId::Privacy,
        group: "Permissions",
        label: "Microphone access",
        keywords: &[
            "voice",
            "audio",
            "dictation",
            "speech",
            "record",
            "permission",
            "permissions",
            "privacy",
            "authorization",
            "allow",
            "denied",
        ],
        selector: "settings-row-microphone-access",
    },
];

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The complete row identity set, so the catalog cannot silently omit one.
    const EVERY_ROW: [SettingsRowId; 14] = [
        SettingsRowId::AppearanceMode,
        SettingsRowId::Transparency,
        SettingsRowId::Blur,
        SettingsRowId::Density,
        SettingsRowId::TerminalTheme,
        SettingsRowId::TerminalFontFamily,
        SettingsRowId::TerminalBaseSize,
        SettingsRowId::TerminalLineHeight,
        SettingsRowId::TerminalRegularWeight,
        SettingsRowId::TerminalBoldWeight,
        SettingsRowId::TerminalItalic,
        SettingsRowId::TerminalBoldAsBright,
        SettingsRowId::InstalledThemes,
        SettingsRowId::MicrophoneAccess,
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

    /// One appearance control governs both surfaces.
    #[test]
    fn one_appearance_mode_governs_both_surfaces() {
        let modes = ROWS
            .iter()
            .filter(|row| matches!(row.id, SettingsRowId::AppearanceMode))
            .count();

        assert_eq!(modes, 1);
    }

    /// The theme row changes the displayed appearance's theme, so its reset leaves the shared
    /// appearance mode and the other slot alone.
    #[test]
    fn the_theme_row_resets_only_the_displayed_theme() {
        for appearance in [Appearance::Light, Appearance::Dark] {
            assert_eq!(
                SettingsRowId::TerminalTheme.reset_target(appearance),
                Some(ResetTarget::TerminalTheme(appearance)),
            );
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
    fn chrome_search_matches_only_appearance_mode() {
        assert_eq!(matching_rows("chrome"), vec![SettingsRowId::AppearanceMode]);
    }

    #[test]
    fn theme_search_matches_only_the_themes_section() {
        assert_eq!(
            matching_rows("theme").into_iter().collect::<HashSet<_>>(),
            HashSet::from([
                SettingsRowId::TerminalTheme,
                SettingsRowId::InstalledThemes,
            ])
        );
    }

    #[test]
    fn terminal_theme_search_reaches_the_current_theme() {
        assert_eq!(
            matching_rows("terminal theme"),
            vec![SettingsRowId::TerminalTheme]
        );
    }

    #[test]
    fn blur_search_matches_only_blur() {
        assert_eq!(matching_rows("blur"), vec![SettingsRowId::Blur]);
    }

    #[test]
    fn interface_search_matches_only_appearance_mode() {
        assert_eq!(
            matching_rows("interface"),
            vec![SettingsRowId::AppearanceMode]
        );
    }

    #[test]
    fn a_keyword_query_reaches_a_row_whose_label_omits_the_word() {
        assert!(matching_rows("leading").contains(&SettingsRowId::TerminalLineHeight));
        assert!(matching_rows("registry").contains(&SettingsRowId::InstalledThemes));
        assert!(matching_rows("import").contains(&SettingsRowId::InstalledThemes));
        assert!(matching_rows("automatic").contains(&SettingsRowId::AppearanceMode));
    }

    /// The diagnostics notice is not a row of its own, so the words a person searches for when a
    /// theme is missing reach the theme selection it describes.
    #[test]
    fn a_missing_theme_query_reaches_the_theme_selection() {
        assert!(matching_rows("fallback").contains(&SettingsRowId::TerminalTheme));
    }

    /// A person whose voice tool cannot hear them searches for what they were doing, not for the
    /// name of the permission.
    #[test]
    fn a_voice_query_reaches_microphone_access() {
        for query in [
            "microphone",
            "Mic",
            "voice",
            "dictation",
            "privacy",
            "permissions",
        ] {
            let matches = matching_rows(query);
            assert_eq!(
                matches.first(),
                Some(&SettingsRowId::MicrophoneAccess),
                "{query:?} should rank microphone access first"
            );
        }
    }

    #[test]
    fn a_section_name_is_not_an_implicit_search_target() {
        let matches = matching_rows("terminal");

        assert!(!matches.contains(&SettingsRowId::TerminalBaseSize));
    }

    #[test]
    fn a_group_title_is_not_an_implicit_search_target() {
        assert!(matching_rows("rendering").is_empty());
    }

    #[test]
    fn label_matches_report_character_indices() {
        let matched = matching_row_matches("line");

        assert_eq!(matched[0].id, SettingsRowId::TerminalLineHeight);
        assert_eq!(matched[0].matched_indices, vec![0, 1, 2, 3]);
    }

    #[test]
    fn non_empty_matches_are_ranked_by_descending_score() {
        let matched = matching_row_matches("font");

        assert!(
            matched
                .windows(2)
                .all(|pair| pair[0].score >= pair[1].score)
        );
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
                SettingsRowId::InstalledThemes | SettingsRowId::MicrophoneAccess
            );
            assert_eq!(
                row.id.reset_target(Appearance::Light).is_some(),
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
