#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalConvention {
    TextInput,
    ControlCharacter,
    Meta,
    ControlNavigation,
}

/// The host desktop's division of keyboard chords between programs in the terminal and
/// application Shortcuts. Composition selects it with the rest of the Desktop Profile.
#[allow(dead_code, reason = "each desktop composition constructs only its own conventions")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalConventions {
    /// Command owns application Shortcuts; every Control and Option chord reaches the terminal.
    CommandShortcuts,
    /// Ctrl+Shift owns application Shortcuts, as in GNOME Terminal, Konsole, and Ptyxis. Super
    /// belongs to the desktop shell.
    ControlShiftShortcuts,
}

impl TerminalConventions {
    /// The modifiers that turn a terminal chord into an application Shortcut.
    pub fn shortcut_modifiers(self) -> gpui::Modifiers {
        match self {
            Self::CommandShortcuts => gpui::Modifiers {
                platform: true,
                ..gpui::Modifiers::default()
            },
            Self::ControlShiftShortcuts => gpui::Modifiers {
                control: true,
                shift: true,
                ..gpui::Modifiers::default()
            },
        }
    }

    /// Why a Shortcut, as written or recorded, cannot be an application Shortcut, if it cannot.
    pub(super) fn reservation(
        self,
        shortcut: &super::Shortcut,
        layout: &crate::platform::keyboard_layout::KeyboardLayout,
    ) -> Option<super::Reservation> {
        match self {
            // Command chords are layout-independent, so both spellings follow one table.
            Self::CommandShortcuts => command_shortcut_reservation(shortcut)
                .or_else(|| command_shortcut_reservation(&shortcut.resolve(layout)))
                .map(super::Reservation::Terminal),
            Self::ControlShiftShortcuts => control_shift_reservation(shortcut, layout),
        }
    }
}

/// Terminal input that every supported desktop reserves on every keyboard layout, so no
/// settings document can assign it. Desktop-specific reservations wait for the Keymap Profile.
pub(super) fn universal_reservation(shortcut: &super::Shortcut) -> Option<TerminalConvention> {
    let modifiers = shortcut.modifiers();
    let key = shortcut.key();
    let layout_free = !modifiers.control
        || (!modifiers.shift
            && ((key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic())
                || key == "space"
                || is_navigation_key(key)));
    if modifiers.platform || !layout_free {
        return None;
    }
    command_shortcut_reservation(shortcut)
}

/// Whether any desktop could present the Shortcut as an application Shortcut.
pub(crate) fn is_presentable(shortcut: &super::Shortcut) -> bool {
    universal_reservation(shortcut).is_none()
}

fn command_shortcut_reservation(shortcut: &super::Shortcut) -> Option<TerminalConvention> {
    let modifiers = shortcut.modifiers();
    let key = shortcut.key();
    if modifiers.platform {
        return None;
    }
    if modifiers.alt {
        return Some(TerminalConvention::Meta);
    }
    if !modifiers.control {
        return Some(TerminalConvention::TextInput);
    }
    if (key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic())
        || matches!(
            key,
            "@" | "[" | "{" | "\\" | "|" | "]" | "}" | "^" | "_" | "/" | "?" | "space"
        )
    {
        return Some(TerminalConvention::ControlCharacter);
    }
    if is_navigation_key(key) {
        return Some(TerminalConvention::ControlNavigation);
    }
    None
}

fn control_shift_reservation(
    shortcut: &super::Shortcut,
    layout: &crate::platform::keyboard_layout::KeyboardLayout,
) -> Option<super::Reservation> {
    let modifiers = shortcut.modifiers();
    let key = shortcut.key();
    if modifiers.platform {
        return Some(super::Reservation::System(
            super::SystemReservation::DesktopShortcut,
        ));
    }
    // Layout resolution consumes Shift into a shifted symbol, so `ctrl-!` is Ctrl+Shift+1.
    let shifted = modifiers.shift || layout.is_shifted_symbol(false, key);
    if modifiers.control && shifted {
        return None;
    }
    let convention = if !modifiers.control && !modifiers.alt {
        TerminalConvention::TextInput
    } else if !modifiers.control {
        TerminalConvention::Meta
    } else if is_navigation_key(key) {
        TerminalConvention::ControlNavigation
    } else {
        TerminalConvention::ControlCharacter
    };
    Some(super::Reservation::Terminal(convention))
}

fn is_navigation_key(key: &str) -> bool {
    matches!(
        key,
        "left"
            | "right"
            | "up"
            | "down"
            | "home"
            | "end"
            | "pageup"
            | "pagedown"
            | "backspace"
            | "delete"
    ) || (key.starts_with('f') && key.len() > 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{Reservation, Shortcut, SystemReservation};
    use crate::platform::keyboard_layout::KeyboardLayout;

    fn us() -> KeyboardLayout {
        crate::platform::keyboard_layout::testing::us()
            .snapshot()
            .unwrap()
    }

    fn reservation(conventions: TerminalConventions, source: &str) -> Option<Reservation> {
        let layout = us();
        conventions.reservation(&Shortcut::parse(source).unwrap(), &layout)
    }

    #[test]
    fn terminal_reserved_table_covers_each_convention_and_shift_variant() {
        let cases: &[(TerminalConvention, &[&str])] = &[
            (
                TerminalConvention::TextInput,
                &["k", "shift-k", "f5", "shift-f20", "left", "shift-enter"],
            ),
            (
                TerminalConvention::ControlCharacter,
                &[
                    "ctrl-c",
                    "ctrl-d",
                    "ctrl-shift-c",
                    "ctrl-z",
                    "ctrl-r",
                    "ctrl-a",
                    "ctrl-e",
                    "ctrl-l",
                    "ctrl-u",
                    "ctrl-k",
                    "ctrl-w",
                    "ctrl-@",
                    "ctrl-[",
                    "ctrl-\\",
                    "ctrl-]",
                    "ctrl-^",
                    "ctrl-_",
                    "ctrl-/",
                    "ctrl-?",
                    "ctrl-space",
                ],
            ),
            (
                TerminalConvention::Meta,
                &[
                    "alt-b",
                    "alt-shift-f",
                    "ctrl-alt-c",
                    "alt-.",
                    "alt-backspace",
                    "alt-f5",
                    "alt-shift-ctrl-tab",
                ],
            ),
            (
                TerminalConvention::ControlNavigation,
                &[
                    "ctrl-left",
                    "ctrl-right",
                    "ctrl-up",
                    "ctrl-down",
                    "ctrl-home",
                    "ctrl-end",
                    "ctrl-pageup",
                    "ctrl-pagedown",
                    "ctrl-backspace",
                    "ctrl-delete",
                    "ctrl-f1",
                    "ctrl-f20",
                ],
            ),
        ];
        for &(convention, keys) in cases {
            for source in keys {
                for source in [(*source).to_owned(), format!("shift-{source}")] {
                    assert_eq!(
                        reservation(TerminalConventions::CommandShortcuts, &source),
                        Some(Reservation::Terminal(convention)),
                        "{source}"
                    );
                    assert_eq!(
                        reservation(TerminalConventions::CommandShortcuts, &format!("cmd-{source}")),
                        None,
                        "{source}"
                    );
                }
            }
        }
    }

    #[test]
    fn universal_reservations_are_reserved_by_every_desktop() {
        for source in [
            "k", "shift-k", "1", "!", "alt-b", "alt-shift-f", "ctrl-c", "ctrl-alt-c", "ctrl-space",
            "ctrl-left", "ctrl-f5", "ctrl-alt-backspace",
        ] {
            let shortcut = Shortcut::parse(source).unwrap();
            assert!(universal_reservation(&shortcut).is_some(), "{source}");
            for conventions in [
                TerminalConventions::CommandShortcuts,
                TerminalConventions::ControlShiftShortcuts,
            ] {
                assert!(reservation(conventions, source).is_some(), "{source}");
            }
        }
        for source in ["cmd-t", "ctrl-shift-t", "ctrl-shift-c", "ctrl-@", "ctrl-!", "ctrl-1", "ctrl-/"] {
            assert_eq!(universal_reservation(&Shortcut::parse(source).unwrap()), None, "{source}");
        }
    }

    #[test]
    fn control_shortcuts_explicitly_allowed_by_policy_remain_assignable() {
        for key in [
            "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", ",", ".", ";", "'", "=", "-", "`",
            "tab", "enter",
        ] {
            for modifier in ["ctrl", "ctrl-shift"] {
                let source = format!("{modifier}-{key}");
                assert_eq!(
                    command_shortcut_reservation(&Shortcut::parse(&source).unwrap()),
                    None,
                    "{source}"
                );
            }
        }
    }

    #[test]
    fn control_shift_is_application_space_and_other_chords_reach_the_terminal() {
        for source in [
            "ctrl-shift-t",
            "ctrl-shift-c",
            "ctrl-shift-alt-n",
            "ctrl-shift-1",
            "ctrl-shift-=",
            "ctrl-shift-,",
            "ctrl-shift-[",
            "ctrl-shift-enter",
            "ctrl-shift-left",
            "ctrl-shift-f5",
        ] {
            assert_eq!(
                reservation(TerminalConventions::ControlShiftShortcuts, source),
                None,
                "{source}"
            );
        }
        for (source, convention) in [
            ("k", TerminalConvention::TextInput),
            ("shift-k", TerminalConvention::TextInput),
            ("f5", TerminalConvention::TextInput),
            ("alt-b", TerminalConvention::Meta),
            ("alt-shift-f", TerminalConvention::Meta),
            ("ctrl-c", TerminalConvention::ControlCharacter),
            ("ctrl-1", TerminalConvention::ControlCharacter),
            ("ctrl-,", TerminalConvention::ControlCharacter),
            ("ctrl-alt-c", TerminalConvention::ControlCharacter),
            ("ctrl-tab", TerminalConvention::ControlCharacter),
            ("ctrl-left", TerminalConvention::ControlNavigation),
            ("ctrl-f5", TerminalConvention::ControlNavigation),
        ] {
            assert_eq!(
                reservation(TerminalConventions::ControlShiftShortcuts, source),
                Some(Reservation::Terminal(convention)),
                "{source}"
            );
        }
        assert_eq!(
            reservation(TerminalConventions::ControlShiftShortcuts, "cmd-t"),
            Some(Reservation::System(SystemReservation::DesktopShortcut))
        );
    }
}
