use crate::keybindings::{Shortcut, SystemReservation, SystemReserved};

pub(crate) fn shortcuts() -> Vec<SystemReserved> {
    use SystemReservation::*;
    [
        ("cmd-c", Copy),
        ("cmd-v", Paste),
        ("cmd-x", Cut),
        ("cmd-z", Undo),
        ("shift-cmd-z", Redo),
        ("cmd-a", SelectAll),
        ("cmd-q", Quit),
        ("cmd-h", Hide),
        ("alt-cmd-h", HideOthers),
        ("cmd-m", Minimize),
        ("alt-cmd-m", MinimizeAll),
        ("ctrl-cmd-f", FullScreen),
        ("cmd-tab", AppSwitcher),
        ("shift-cmd-tab", AppSwitcher),
        ("cmd-`", WindowCycling),
        ("shift-cmd-`", WindowCycling),
        ("cmd-space", Spotlight),
        ("alt-cmd-space", Spotlight),
        ("ctrl-cmd-space", CharacterViewer),
        ("alt-cmd-escape", ForceQuit),
        ("ctrl-cmd-q", LockScreen),
        ("shift-cmd-q", LogOut),
        ("alt-shift-cmd-q", LogOut),
        ("shift-cmd-3", Screenshot),
        ("ctrl-shift-cmd-3", Screenshot),
        ("shift-cmd-4", Screenshot),
        ("ctrl-shift-cmd-4", Screenshot),
        ("shift-cmd-5", Screenshot),
        ("ctrl-shift-cmd-5", Screenshot),
        ("shift-cmd-6", Screenshot),
        ("ctrl-shift-cmd-6", Screenshot),
        ("shift-cmd-/", Help),
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
    use crate::keybindings::{Command, KeybindingPreferences, Reservation};

    #[test]
    fn system_reservations_allow_the_complete_default_profile() {
        let profile = crate::desktop_profile::default_keymap::profile(shortcuts()).unwrap();
        let resolved = profile.resolve(&KeybindingPreferences::default());
        assert_eq!(resolved.shortcut(Command::CloseWorkspace), None);
        for command in Command::ALL {
            if command != Command::CloseWorkspace {
                assert!(resolved.shortcut(command).is_some(), "{command:?}");
            }
            for shortcut in resolved.shortcuts(command) {
                assert_eq!(profile.check(shortcut), Ok(()));
            }
        }
        for reserved in shortcuts() {
            assert_eq!(
                profile.check(&reserved.shortcut),
                Err(Reservation::System(reserved.reason))
            );
        }
    }
}
