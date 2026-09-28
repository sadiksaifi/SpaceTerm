#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalConvention {
    TextInput,
    ControlCharacter,
    Meta,
    ControlNavigation,
}

pub(super) fn reservation(shortcut: &super::Shortcut) -> Option<TerminalConvention> {
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
            "@" | "[" | "\\" | "]" | "^" | "_" | "/" | "?" | "space"
        )
    {
        return Some(TerminalConvention::ControlCharacter);
    }
    if matches!(
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
    {
        return Some(TerminalConvention::ControlNavigation);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{Shortcut, ShortcutRejection};

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
                        Shortcut::parse(&source),
                        Err(ShortcutRejection::TerminalReserved(convention)),
                        "{source}"
                    );
                    let keystroke = gpui::Keystroke::parse(&source).unwrap();
                    assert_eq!(
                        Shortcut::from_keystroke(&keystroke),
                        Err(ShortcutRejection::TerminalReserved(convention)),
                        "{source}"
                    );
                    assert!(Shortcut::parse(&format!("cmd-{source}")).is_ok());
                }
            }
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
                assert!(Shortcut::parse(&source).is_ok(), "{source}");
                assert!(
                    Shortcut::from_keystroke(&gpui::Keystroke::parse(&source).unwrap()).is_ok(),
                    "{source}"
                );
            }
        }
    }
}
