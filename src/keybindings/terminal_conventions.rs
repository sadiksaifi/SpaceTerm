#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalConvention {
    TextInput,
    ControlCharacter,
    Meta,
    ControlNavigation,
}

/// The host desktop's division of keyboard chords between programs in the terminal and
/// application Shortcuts. Composition selects it with the rest of the Desktop Profile.
#[allow(
    dead_code,
    reason = "each desktop composition constructs only its own conventions"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalConventions {
    /// Command owns application Shortcuts; every Control and Option chord reaches the terminal.
    CommandShortcuts,
    /// Ctrl+Shift owns application Shortcuts, as in GNOME Terminal, Konsole, and Ptyxis, together
    /// with the few chords outside it that those terminals also give the application. Super
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

/// Terminal input that every supported desktop reserves on every keyboard layout. Retained
/// settings can contain it, but the Keymap Profile prevents installing it as an application chord.
pub(super) fn universal_reservation(shortcut: &super::Shortcut) -> Option<TerminalConvention> {
    let modifiers = shortcut.modifiers();
    let key = shortcut.key();
    let layout_free = !modifiers.control
        || (!modifiers.shift
            && ((key.len() == 1 && key.as_bytes()[0].is_ascii_alphabetic())
                || key == "space"
                || is_navigation_key(key)));
    if modifiers.platform || !layout_free || is_control_shift_extension(modifiers, key) {
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
    if (modifiers.control && shifted) || is_control_shift_extension(modifiers, key) {
        return None;
    }
    let convention = if !modifiers.control && !modifiers.alt {
        TerminalConvention::TextInput
    } else if !modifiers.control {
        TerminalConvention::Meta
    } else if key.chars().count() > 1 && key != "space" {
        TerminalConvention::ControlNavigation
    } else {
        TerminalConvention::ControlCharacter
    };
    Some(super::Reservation::Terminal(convention))
}

/// The chords outside Ctrl+Shift that VTE terminals and Konsole give to the application. Each is
/// written as GPUI dispatches it, so a shifted symbol such as `ctrl-+` on US English is already
/// Ctrl+Shift.
/// Plain Ctrl+letter and Alt+letter, the unshifted xterm control forms such as Ctrl+2 and Ctrl+[,
/// and Super stay outside it.
fn is_control_shift_extension(modifiers: gpui::Modifiers, key: &str) -> bool {
    if modifiers.platform {
        return false;
    }
    let plain_control = modifiers.control && !modifiers.alt && !modifiers.shift;
    // These send no control code programs rely on: xterm sends the bare symbol or digit for the
    // first five, desktop terminals change Tabs with Ctrl+PageUp, Ctrl+PageDown, and Ctrl+Tab, and
    // Ctrl+function keys carry no terminal convention.
    let control_application_key = plain_control
        && (matches!(
            key,
            "=" | "+" | "-" | "0" | "," | "pageup" | "pagedown" | "tab"
        ) || is_function_key(key));
    // Ctrl+Insert copies, as in the X11 tradition these terminals keep.
    let control_insert = plain_control && key == "insert";
    // Alt with an unshifted digit selects a Tab in GNOME Terminal and Konsole, and the digit keeps
    // its identity across keyboard layouts.
    let alt_digit =
        modifiers.alt && !modifiers.shift && key.len() == 1 && key.as_bytes()[0].is_ascii_digit();
    // Shift with Insert and the paging keys is the terminal's own paste and Scrollback control.
    let shift_navigation = modifiers.shift
        && !modifiers.control
        && !modifiers.alt
        && matches!(key, "insert" | "pageup" | "pagedown" | "home" | "end");
    // Function keys alone or with Shift, which desktop terminals bind as F11 is for full screen.
    let function_key = !modifiers.control && !modifiers.alt && is_function_key(key);
    control_application_key || control_insert || alt_digit || shift_navigation || function_key
}

fn is_function_key(key: &str) -> bool {
    key.strip_prefix('f').is_some_and(|number| {
        !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit())
    })
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
            .snapshot(&crate::platform::keyboard_layout::testing::UnknownLayout)
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
                        reservation(
                            TerminalConventions::CommandShortcuts,
                            &format!("cmd-{source}")
                        ),
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
            "k",
            "shift-k",
            "1",
            "!",
            "alt-b",
            "alt-shift-f",
            "ctrl-c",
            "ctrl-alt-c",
            "ctrl-space",
            "ctrl-left",
            "ctrl-home",
            "ctrl-alt-backspace",
            "alt-f4",
            "shift-left",
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
        for source in [
            "cmd-t",
            "ctrl-shift-t",
            "ctrl-shift-c",
            "ctrl-@",
            "ctrl-!",
            "ctrl-1",
            "ctrl-/",
            // Control-Shift desktops give these to the application, so they can be presented.
            "alt-1",
            "ctrl-pageup",
            "ctrl-tab",
            "ctrl-f5",
            "shift-pageup",
            "shift-insert",
            "f3",
            "shift-f3",
        ] {
            let shortcut = Shortcut::parse(source).unwrap();
            assert_eq!(universal_reservation(&shortcut), None, "{source}");
            assert!(is_presentable(&shortcut), "{source}");
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
            ("alt-b", TerminalConvention::Meta),
            ("alt-shift-f", TerminalConvention::Meta),
            ("ctrl-c", TerminalConvention::ControlCharacter),
            ("ctrl-1", TerminalConvention::ControlCharacter),
            ("ctrl-alt-c", TerminalConvention::ControlCharacter),
            ("ctrl-enter", TerminalConvention::ControlNavigation),
            ("ctrl-space", TerminalConvention::ControlCharacter),
            ("ctrl-left", TerminalConvention::ControlNavigation),
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

    #[test]
    fn control_shift_desktops_give_the_application_the_chords_their_terminals_leave_free() {
        for source in [
            // Plain Ctrl with keys that send no control code programs rely on.
            "ctrl-=",
            "ctrl-+",
            "ctrl--",
            "ctrl-0",
            "ctrl-,",
            "ctrl-pageup",
            "ctrl-pagedown",
            "ctrl-tab",
            "ctrl-f1",
            "ctrl-f12",
            "ctrl-insert",
            // Alt and Ctrl+Alt with an unshifted digit.
            "alt-1",
            "alt-9",
            "alt-0",
            "ctrl-alt-1",
            "ctrl-alt-9",
            // Shift with Insert and the paging keys, and function keys alone or with Shift.
            "shift-insert",
            "shift-pageup",
            "shift-pagedown",
            "shift-home",
            "shift-end",
            "f3",
            "f9",
            "f20",
            "shift-f3",
        ] {
            assert_eq!(
                reservation(TerminalConventions::ControlShiftShortcuts, source),
                None,
                "{source}"
            );
        }
        for (source, convention) in [
            // Plain Ctrl+letter, the unshifted xterm control forms, and Ctrl+arrows stay with
            // programs in the terminal.
            ("ctrl-a", TerminalConvention::ControlCharacter),
            ("ctrl-2", TerminalConvention::ControlCharacter),
            ("ctrl-6", TerminalConvention::ControlCharacter),
            ("ctrl-[", TerminalConvention::ControlCharacter),
            ("ctrl-]", TerminalConvention::ControlCharacter),
            ("ctrl-/", TerminalConvention::ControlCharacter),
            ("ctrl-home", TerminalConvention::ControlNavigation),
            ("ctrl-delete", TerminalConvention::ControlNavigation),
            ("ctrl-alt-=", TerminalConvention::ControlCharacter),
            // Alt+letter, Alt with a shifted digit, and Alt with function keys or Tab.
            ("alt-a", TerminalConvention::Meta),
            ("alt-!", TerminalConvention::Meta),
            ("alt-shift-1", TerminalConvention::Meta),
            ("alt-f4", TerminalConvention::Meta),
            ("alt-f10", TerminalConvention::Meta),
            ("alt-tab", TerminalConvention::Meta),
            // Desktop chords outside Ctrl+Shift.
            ("ctrl-alt-left", TerminalConvention::ControlNavigation),
            ("ctrl-alt-f1", TerminalConvention::ControlNavigation),
            ("ctrl-alt-f12", TerminalConvention::ControlNavigation),
            // Shift with arrows, Tab, or Delete still types into the terminal.
            ("shift-left", TerminalConvention::TextInput),
            ("shift-tab", TerminalConvention::TextInput),
            ("shift-delete", TerminalConvention::TextInput),
            ("insert", TerminalConvention::TextInput),
            ("pageup", TerminalConvention::TextInput),
        ] {
            assert_eq!(
                reservation(TerminalConventions::ControlShiftShortcuts, source),
                Some(Reservation::Terminal(convention)),
                "{source}"
            );
        }
    }

    #[test]
    fn a_symbol_available_without_shift_stays_terminal_reserved() {
        let mut layout = KeyboardLayout::default();
        layout.insert(false, ",", ";");
        layout.insert(false, ";", ".");
        assert_eq!(
            TerminalConventions::ControlShiftShortcuts
                .reservation(&Shortcut::parse("ctrl-;").unwrap(), &layout),
            Some(Reservation::Terminal(TerminalConvention::ControlCharacter)),
        );
    }
}
