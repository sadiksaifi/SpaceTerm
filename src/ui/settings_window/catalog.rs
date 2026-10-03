//! The identity, order, and search vocabulary of every Settings Row.
//!
//! One table owns row identity so navigation, search, rendering, and reset all agree. A row that
//! is not listed here cannot be found by Settings Search, so the suite asserts that every
//! [`SettingsRowId`] appears exactly once.

use spaceterm_ui::{FuzzyTarget, fuzzy_filter};

use crate::appearance::{Appearance, ResetTarget};
use crate::keybindings::{Command, CommandGroup};

/// One named group of Settings presented as one navigation entry and one content region.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum SettingsSectionId {
    /// How SpaceTerm's windows present themselves: density, transparency, and blur.
    Interface,
    /// Terminal typography and text rendering.
    Font,
    /// The light, dark, or automatic appearance, and the Terminal Theme each appearance uses.
    Themes,
    /// The Shortcut each Command resolves to.
    Keybindings,
    /// Clipboard access and system permissions for terminal programs.
    Privacy,
    /// The installed version, the latest check, and how SpaceTerm keeps itself current.
    Updates,
    /// Settings as a whole: the settings file, export and import, and Reset All.
    Advanced,
}

impl SettingsSectionId {
    /// Every section in presentation order.
    pub(super) const ALL: [Self; 7] = [
        Self::Interface,
        Self::Font,
        Self::Themes,
        Self::Keybindings,
        Self::Privacy,
        Self::Updates,
        Self::Advanced,
    ];

    pub(super) const fn title(self) -> &'static str {
        match self {
            Self::Interface => "Interface",
            Self::Font => "Font",
            Self::Themes => "Themes",
            Self::Keybindings => "Keybindings",
            Self::Privacy => "Privacy",
            Self::Updates => "Updates",
            Self::Advanced => "Advanced",
        }
    }

    pub(super) const fn description(self) -> &'static str {
        match self {
            Self::Interface => "Density, transparency, and blur for SpaceTerm's windows.",
            Self::Font => "The typeface and text rendering in terminal panes.",
            Self::Themes => {
                "Light, dark, or automatic appearance, and the colors terminal panes use in each. Themes published for Zed work too."
            }
            Self::Keybindings => {
                "Shortcuts for SpaceTerm commands. Click a shortcut, then press the new keys. Press Delete to remove it."
            }
            Self::Privacy => {
                "Clipboard access and system permissions for programs running in SpaceTerm."
            }
            Self::Updates => {
                "SpaceTerm keeps itself current in the background and asks before it restarts."
            }
            Self::Advanced => {
                "Every setting at once: edit them as JSON, export or import them, or start over."
            }
        }
    }

    pub(super) const fn selector(self) -> &'static str {
        match self {
            Self::Interface => "settings-section-interface",
            Self::Font => "settings-section-font",
            Self::Themes => "settings-section-themes",
            Self::Keybindings => "settings-section-keybindings",
            Self::Privacy => "settings-section-privacy",
            Self::Updates => "settings-section-updates",
            Self::Advanced => "settings-section-advanced",
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
    /// The system's Screen Recording authorization, which computer-use tools in a Terminal Session
    /// inherit to take screenshots.
    ScreenRecordingAccess,
    /// The system's Accessibility authorization, which the system presents as Device Control and Data
    /// Access and computer-use tools in a Terminal Session inherit to click and type.
    DeviceControlAccess,
    ClipboardWrites,
    ClipboardReads,
    /// The installed version, the latest check, and the next step the update service offers.
    UpdateStatus,
    AutomaticUpdateDownloads,
    UpdateCheckInterval,
    /// How often an overdue update is brought back after it was dismissed.
    UpdateReminderInterval,
    /// The Keybinding of one Command.
    Shortcut(Command),
    /// The settings file, shown read-only and edited in the person's own editor.
    SettingsFile,
    /// Writes the Settings Document to a file the person chooses.
    ExportSettings,
    /// Replaces the Settings Document with one read from a file the person chooses.
    ImportSettings,
    /// Returns every Setting to its default and removes the installed Terminal Themes.
    ResetAllSettings,
}

impl SettingsRowId {
    pub(super) fn descriptor(self) -> &'static SettingsRowDescriptor {
        rows()
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
            // Update preferences live outside the appearance preferences, so their rows reset
            // through their own field instead of an appearance reset target.
            Self::InstalledThemes
            | Self::MicrophoneAccess
            | Self::ScreenRecordingAccess
            | Self::DeviceControlAccess
            | Self::ClipboardWrites
            | Self::ClipboardReads
            | Self::UpdateStatus
            | Self::AutomaticUpdateDownloads
            | Self::UpdateCheckInterval
            | Self::UpdateReminderInterval
            // Keybindings live outside the appearance preferences and reset through the keymap
            // profile, which returns a displaced default to its Command.
            | Self::Shortcut(_)
            // The Advanced rows act on Settings as a whole and hold no preference of their own.
            | Self::SettingsFile
            | Self::ExportSettings
            | Self::ImportSettings
            | Self::ResetAllSettings => {
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
    let rows = rows().collect::<Vec<_>>();
    let fields = rows
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
    let mut seen_rows = vec![false; rows.len()];

    fuzzy_filter(&fields, query, |(_, _, text)| FuzzyTarget::new(text))
        .into_iter()
        .filter_map(|matched| {
            let (row_index, is_label, _) = fields[matched.item_index()];
            if std::mem::replace(&mut seen_rows[row_index], true) {
                return None;
            }
            Some(SettingsRowMatch {
                id: rows[row_index].id,
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

/// Every row in table order: the preference rows, then one row per Command.
pub(super) fn rows() -> impl Iterator<Item = &'static SettingsRowDescriptor> + Clone {
    PREFERENCE_ROWS.iter().chain(SHORTCUT_ROWS)
}

const PREFERENCE_ROWS: &[SettingsRowDescriptor] = &[
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
    SettingsRowDescriptor {
        id: SettingsRowId::ScreenRecordingAccess,
        section: SettingsSectionId::Privacy,
        group: "Permissions",
        label: "Screen recording access",
        keywords: &[
            "computer use",
            "screenshot",
            "screen capture",
            "capture",
            "agent",
            "permission",
            "permissions",
            "privacy",
            "authorization",
            "allow",
            "denied",
            "troubleshoot",
            "reset",
        ],
        selector: "settings-row-screen-recording-access",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::DeviceControlAccess,
        section: SettingsSectionId::Privacy,
        group: "Permissions",
        label: "Device control access",
        keywords: &[
            "accessibility",
            "computer use",
            "data access",
            "click",
            "type",
            "input",
            "automation",
            "agent",
            "permission",
            "permissions",
            "privacy",
            "authorization",
            "allow",
            "denied",
            "troubleshoot",
            "reset",
        ],
        selector: "settings-row-device-control-access",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ClipboardWrites,
        section: SettingsSectionId::Privacy,
        group: "Clipboard",
        label: "Allow terminal programs to copy text",
        keywords: &[
            "clipboard",
            "copy",
            "paste",
            "ssh",
            "osc 52",
            "neovim",
            "tui",
        ],
        selector: "settings-row-clipboard-writes",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ClipboardReads,
        section: SettingsSectionId::Privacy,
        group: "Clipboard",
        label: "Allow terminal programs to read clipboard text",
        keywords: &[
            "clipboard",
            "copy",
            "paste",
            "ssh",
            "osc 52",
            "neovim",
            "tui",
        ],
        selector: "settings-row-clipboard-reads",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::UpdateStatus,
        section: SettingsSectionId::Updates,
        // The installed version leads the page and needs no title: it is what the page is about.
        group: "",
        label: super::updates::CURRENT_VERSION_LABEL,
        keywords: &[
            "version",
            "check",
            "check now",
            "check for updates",
            "last checked",
            "update",
            "upgrade",
            "restart",
            "install",
            "release",
            "about",
        ],
        selector: "settings-row-update-status",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::AutomaticUpdateDownloads,
        section: SettingsSectionId::Updates,
        group: "Automatic updates",
        label: "Download updates automatically",
        keywords: &["automatic", "background", "download", "update", "auto"],
        selector: "settings-row-automatic-update-downloads",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::UpdateCheckInterval,
        section: SettingsSectionId::Updates,
        group: "Automatic updates",
        label: "Check for updates",
        keywords: &[
            "frequency",
            "interval",
            "hourly",
            "daily",
            "schedule",
            "update",
        ],
        selector: "settings-row-update-check-interval",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::UpdateReminderInterval,
        section: SettingsSectionId::Updates,
        group: "Automatic updates",
        label: "Remind me about overdue updates",
        keywords: &[
            "reminder",
            "remind",
            "overdue",
            "banner",
            "notification",
            "frequency",
            "interval",
            "update",
        ],
        selector: "settings-row-update-reminder-interval",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::SettingsFile,
        section: SettingsSectionId::Advanced,
        group: "Settings File",
        label: "Settings file",
        keywords: &[
            "json", "edit", "editor", "source", "raw", "text", "file", "path", "location",
            "reload", "refresh", "advanced",
        ],
        selector: "settings-row-settings-file",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ExportSettings,
        section: SettingsSectionId::Advanced,
        group: "Transfer",
        label: "Export settings",
        keywords: &[
            "export", "backup", "save", "copy", "file", "transfer", "migrate",
        ],
        selector: "settings-row-export-settings",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ImportSettings,
        section: SettingsSectionId::Advanced,
        group: "Transfer",
        label: "Import settings",
        keywords: &[
            "import", "restore", "backup", "load", "file", "transfer", "migrate",
        ],
        selector: "settings-row-import-settings",
    },
    SettingsRowDescriptor {
        id: SettingsRowId::ResetAllSettings,
        section: SettingsSectionId::Advanced,
        group: "Reset",
        label: "Reset all settings",
        keywords: &[
            "reset",
            "defaults",
            "factory",
            "clear",
            "start over",
            "restore",
        ],
        selector: "settings-row-reset-all-settings",
    },
];

/// The titled box each Command's row shares with the other Commands of its group.
const fn shortcut_group(group: CommandGroup) -> &'static str {
    match group {
        CommandGroup::Workspace => "Workspaces",
        CommandGroup::Tab => "Tabs",
        CommandGroup::Pane => "Panes",
        CommandGroup::Terminal => "Terminal",
        CommandGroup::View => "View",
    }
}

const SHORTCUT_KEYWORDS: &[&str] = &["shortcut", "keyboard", "keybinding", "hotkey"];

/// One row per Command, in [`Command::ALL`] order, which already runs group by group.
macro_rules! shortcut_rows {
    ($($command:ident => $slug:literal),+ $(,)?) => {
        const SHORTCUT_ROWS: &[SettingsRowDescriptor] = &[
            $(
                SettingsRowDescriptor {
                    id: SettingsRowId::Shortcut(Command::$command),
                    section: SettingsSectionId::Keybindings,
                    group: shortcut_group(Command::$command.group()),
                    label: Command::$command.label(),
                    keywords: SHORTCUT_KEYWORDS,
                    selector: concat!("settings-row-shortcut-", $slug),
                },
            )+
        ];
    };
}

shortcut_rows! {
    SwitchWorkspace => "switch-workspace",
    NewWorkspace => "new-workspace",
    NewRemoteWorkspace => "new-remote-workspace",
    OpenLocalDirectory => "open-local-directory",
    OpenRemoteDirectory => "open-remote-directory",
    CloseWorkspace => "close-workspace",
    ActivateWorkspace1 => "activate-workspace-1",
    ActivateWorkspace2 => "activate-workspace-2",
    ActivateWorkspace3 => "activate-workspace-3",
    ActivateWorkspace4 => "activate-workspace-4",
    ActivateWorkspace5 => "activate-workspace-5",
    ActivateWorkspace6 => "activate-workspace-6",
    ActivateWorkspace7 => "activate-workspace-7",
    ActivateWorkspace8 => "activate-workspace-8",
    ActivateWorkspace9 => "activate-workspace-9",
    CreateTab => "create-tab",
    CloseTab => "close-tab",
    ActivateTab1 => "activate-tab-1",
    ActivateTab2 => "activate-tab-2",
    ActivateTab3 => "activate-tab-3",
    ActivateTab4 => "activate-tab-4",
    ActivateTab5 => "activate-tab-5",
    ActivateTab6 => "activate-tab-6",
    ActivateTab7 => "activate-tab-7",
    ActivateTab8 => "activate-tab-8",
    ActivateTab9 => "activate-tab-9",
    ClosePane => "close-pane",
    SplitRight => "split-right",
    SplitDown => "split-down",
    FocusPaneLeft => "focus-pane-left",
    FocusPaneRight => "focus-pane-right",
    FocusPaneUp => "focus-pane-up",
    FocusPaneDown => "focus-pane-down",
    FocusPreviousPane => "focus-previous-pane",
    FocusNextPane => "focus-next-pane",
    TogglePaneZoom => "toggle-pane-zoom",
    OpenTerminalFind => "open-terminal-find",
    FindNext => "find-next",
    FindPrevious => "find-previous",
    ClearTerminalScreenAndScrollback => "clear-terminal-screen-and-scrollback",
    IncreaseTerminalFontSize => "increase-terminal-font-size",
    DecreaseTerminalFontSize => "decrease-terminal-font-size",
    ResetTerminalFontSize => "reset-terminal-font-size",
    ToggleSidebar => "toggle-sidebar",
    ToggleSidebarFocus => "toggle-sidebar-focus",
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The complete preference row identity set, so the catalog cannot silently omit one.
    const EVERY_PREFERENCE_ROW: [SettingsRowId; 26] = [
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
        SettingsRowId::ScreenRecordingAccess,
        SettingsRowId::DeviceControlAccess,
        SettingsRowId::ClipboardWrites,
        SettingsRowId::ClipboardReads,
        SettingsRowId::UpdateStatus,
        SettingsRowId::AutomaticUpdateDownloads,
        SettingsRowId::UpdateCheckInterval,
        SettingsRowId::UpdateReminderInterval,
        SettingsRowId::SettingsFile,
        SettingsRowId::ExportSettings,
        SettingsRowId::ImportSettings,
        SettingsRowId::ResetAllSettings,
    ];

    fn every_row() -> Vec<SettingsRowId> {
        EVERY_PREFERENCE_ROW
            .into_iter()
            .chain(Command::ALL.map(SettingsRowId::Shortcut))
            .collect()
    }

    #[test]
    fn every_row_identity_is_listed_exactly_once() {
        let every_row = every_row();
        for &id in &every_row {
            let listed = rows().filter(|row| row.id == id).count();
            assert_eq!(listed, 1, "{id:?} should be listed exactly once");
        }
        assert_eq!(rows().count(), every_row.len());
    }

    #[test]
    fn shortcut_rows_follow_command_order_in_the_keybindings_section() {
        assert!(
            SHORTCUT_ROWS
                .iter()
                .map(|row| row.id)
                .eq(Command::ALL.map(SettingsRowId::Shortcut))
        );
        for row in SHORTCUT_ROWS {
            let SettingsRowId::Shortcut(command) = row.id else {
                unreachable!()
            };
            assert_eq!(row.section, SettingsSectionId::Keybindings);
            assert_eq!(row.label, command.label());
        }
    }

    #[test]
    fn every_row_selector_is_unique() {
        let selectors = rows().map(|row| row.selector).collect::<HashSet<_>>();

        assert_eq!(selectors.len(), rows().count());
    }

    #[test]
    fn every_row_belongs_to_a_presented_section() {
        for row in rows() {
            assert!(SettingsSectionId::ALL.contains(&row.section));
        }
    }

    #[test]
    fn every_section_presents_at_least_one_row() {
        for section in SettingsSectionId::ALL {
            assert!(
                rows().any(|row| row.section == section),
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
        for row in rows() {
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
        let modes = rows()
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
        assert_eq!(matching_rows("   ").len(), rows().count());
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
            HashSet::from([SettingsRowId::TerminalTheme, SettingsRowId::InstalledThemes,])
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

    /// A person whose computer-use tool cannot see or control the screen searches for what the
    /// tool does or for the permission's name in System Settings.
    #[test]
    fn a_computer_use_query_reaches_its_permission() {
        for (query, row) in [
            ("screen recording", SettingsRowId::ScreenRecordingAccess),
            ("screenshot", SettingsRowId::ScreenRecordingAccess),
            ("accessibility", SettingsRowId::DeviceControlAccess),
            ("device control", SettingsRowId::DeviceControlAccess),
            ("click", SettingsRowId::DeviceControlAccess),
        ] {
            assert_eq!(
                matching_rows(query).first(),
                Some(&row),
                "{query:?} should rank {row:?} first"
            );
        }
        let computer_use = matching_rows("computer use");
        for row in [
            SettingsRowId::ScreenRecordingAccess,
            SettingsRowId::DeviceControlAccess,
        ] {
            assert!(
                computer_use.contains(&row),
                "computer use should reach {row:?}"
            );
        }
    }

    #[test]
    fn a_command_query_reaches_its_shortcut_row() {
        assert_eq!(
            matching_rows("close tab").first(),
            Some(&SettingsRowId::Shortcut(Command::CloseTab))
        );
        assert_eq!(
            matching_rows("split right").first(),
            Some(&SettingsRowId::Shortcut(Command::SplitRight))
        );
    }

    #[test]
    fn a_shortcut_query_reaches_every_command() {
        for query in ["shortcut", "keyboard", "hotkey"] {
            let matches = matching_rows(query);
            for command in Command::ALL {
                assert!(
                    matches.contains(&SettingsRowId::Shortcut(command)),
                    "{query:?} should reach {command:?}"
                );
            }
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
    fn every_appearance_preference_row_maps_to_a_reset_target() {
        for row in rows() {
            // Update and shortcut rows reset their own field; their section tests cover that path.
            let resettable = !matches!(
                row.id,
                SettingsRowId::InstalledThemes
                    | SettingsRowId::MicrophoneAccess
                    | SettingsRowId::ScreenRecordingAccess
                    | SettingsRowId::DeviceControlAccess
                    | SettingsRowId::ClipboardWrites
                    | SettingsRowId::ClipboardReads
                    | SettingsRowId::UpdateStatus
                    | SettingsRowId::AutomaticUpdateDownloads
                    | SettingsRowId::UpdateCheckInterval
                    | SettingsRowId::UpdateReminderInterval
                    | SettingsRowId::Shortcut(_)
                    | SettingsRowId::SettingsFile
                    | SettingsRowId::ExportSettings
                    | SettingsRowId::ImportSettings
                    | SettingsRowId::ResetAllSettings
            );
            assert_eq!(
                row.id.reset_target(Appearance::Light).is_some(),
                resettable,
                "{:?} reset mapping disagrees with its kind",
                row.id
            );
        }
    }

    /// The Advanced rows answer what a person looking to act on all of Settings types.
    #[test]
    fn a_whole_settings_query_reaches_the_advanced_rows() {
        for (query, row) in [
            ("json", SettingsRowId::SettingsFile),
            ("export", SettingsRowId::ExportSettings),
            ("backup", SettingsRowId::ExportSettings),
            ("restore", SettingsRowId::ImportSettings),
            ("reset all", SettingsRowId::ResetAllSettings),
            ("factory", SettingsRowId::ResetAllSettings),
        ] {
            assert!(
                matching_rows(query).contains(&row),
                "{query:?} should reach {row:?}"
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
