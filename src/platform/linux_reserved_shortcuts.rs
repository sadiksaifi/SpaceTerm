//! Chords that Linux desktops, input methods, or SpaceTerm's fixed bindings already own inside
//! the Ctrl+Shift application space. Chords outside that space are terminal-reserved by the
//! Linux terminal conventions and need no entry.
use crate::keybindings::{Shortcut, SystemReservation, SystemReserved};

/// The Keymap Profile resolves each spelling to the chord GPUI dispatches on the active layout.
pub(super) fn shortcuts() -> Vec<SystemReserved> {
    use SystemReservation::*;
    [
        ("ctrl-shift-c", Copy),
        ("ctrl-shift-v", Paste),
        ("shift-insert", PasteSelection),
        ("ctrl-shift-q", Quit),
        ("ctrl-shift-m", Minimize),
        ("ctrl-shift-,", Settings),
        ("f11", FullScreen),
        ("ctrl-shift-alt-left", MoveWindowToWorkspace),
        ("ctrl-shift-alt-right", MoveWindowToWorkspace),
        ("ctrl-shift-alt-up", MoveWindowToWorkspace),
        ("ctrl-shift-alt-down", MoveWindowToWorkspace),
        ("ctrl-shift-alt-r", ScreenRecording),
        ("ctrl-shift-alt-tab", KeyboardNavigation),
        ("ctrl-shift-alt-escape", KeyboardNavigation),
        ("ctrl-shift-u", InputMethod),
        ("ctrl-shift-alt-u", InputMethod),
        ("ctrl-shift-alt-delete", LogOut),
        ("ctrl-shift-alt-pageup", Restart),
        ("ctrl-shift-alt-pagedown", ShutDown),
    ]
    .into_iter()
    .map(|(source, reason)| SystemReserved {
        shortcut: Shortcut::parse(source).expect("valid static system shortcut"),
        reason,
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::{Command, KeybindingPreferences, Reservation, TerminalConvention};

    fn profile() -> crate::keybindings::KeymapProfile {
        super::super::linux_default_keymap::profile(
            std::rc::Rc::new(super::super::linux_keyboard_layout::LinuxKeyboardLayout),
            shortcuts(),
        )
        .unwrap()
    }

    #[test]
    fn linux_reserved_chords_block_assignment_in_their_dispatch_spelling() {
        let profile = profile();
        for (source, reason) in [
            ("ctrl-shift-,", SystemReservation::Settings),
            ("ctrl-<", SystemReservation::Settings),
            ("ctrl-shift-c", SystemReservation::Copy),
            ("ctrl-shift-m", SystemReservation::Minimize),
            ("ctrl-shift-u", SystemReservation::InputMethod),
            (
                "ctrl-shift-alt-left",
                SystemReservation::MoveWindowToWorkspace,
            ),
        ] {
            assert_eq!(
                profile.check(&Shortcut::parse(source).unwrap()),
                Err(Reservation::System(reason)),
                "{source}"
            );
        }
        let mut preferences = KeybindingPreferences::default();
        assert!(
            profile
                .assign(
                    &mut preferences,
                    Command::NewWorkspace,
                    Some(Shortcut::parse("ctrl-shift-q").unwrap())
                )
                .is_err()
        );
    }

    #[test]
    fn linux_conventions_leave_plain_control_and_alt_to_the_terminal() {
        let profile = profile();
        for (source, expected) in [
            (
                "ctrl-c",
                Reservation::Terminal(TerminalConvention::ControlCharacter),
            ),
            (
                "ctrl-left",
                Reservation::Terminal(TerminalConvention::ControlNavigation),
            ),
            ("alt-f", Reservation::Terminal(TerminalConvention::Meta)),
            ("a", Reservation::Terminal(TerminalConvention::TextInput)),
            (
                "cmd-t",
                Reservation::System(SystemReservation::DesktopShortcut),
            ),
        ] {
            assert_eq!(
                profile.check(&Shortcut::parse(source).unwrap()),
                Err(expected),
                "{source}"
            );
        }
        for source in [
            "ctrl-shift-t",
            "ctrl-shift-1",
            "ctrl-shift-alt-n",
            "ctrl-=",
            "ctrl-pageup",
            "alt-1",
            "ctrl-alt-1",
            "shift-pageup",
            "f9",
        ] {
            assert_eq!(profile.check(&Shortcut::parse(source).unwrap()), Ok(()));
        }
    }

    #[test]
    fn linux_reservations_apply_before_and_after_a_host_layout_arrives() {
        use crate::platform::keyboard_layout::{KeyboardLayout, testing};
        for layout in [
            KeyboardLayout::default(),
            KeyboardLayout::us_english(),
            testing::de(),
            testing::fr_azerty(),
        ] {
            let mut profile =
                super::super::linux_default_keymap::profile(std::rc::Rc::new(layout), shortcuts())
                    .unwrap();
            profile.refresh_layout(&testing::UnknownLayout).unwrap();
            for reserved in shortcuts() {
                if !reserved.shortcut.modifiers().control {
                    continue;
                }
                for shortcut in [
                    reserved.shortcut.clone(),
                    reserved.shortcut.resolve(profile.layout()),
                ] {
                    assert_eq!(
                        profile.check(&shortcut),
                        Err(Reservation::System(reserved.reason)),
                        "{shortcut}"
                    );
                }
            }
            for key in ["ctrl-space", "ctrl-2", "ctrl-6", "ctrl-/"] {
                let shortcut = Shortcut::parse(key).unwrap();
                // On AZERTY digits are Shift-only. Unshifted terminal symbols stay reserved.
                if !profile.layout().is_shifted_symbol(false, shortcut.key()) {
                    assert_eq!(
                        profile.check(&shortcut),
                        Err(Reservation::Terminal(TerminalConvention::ControlCharacter)),
                        "{key}"
                    );
                }
            }
            assert_eq!(
                profile.check(&Shortcut::parse("cmd-shift-t").unwrap()),
                Err(Reservation::System(SystemReservation::DesktopShortcut))
            );
        }
    }
}
