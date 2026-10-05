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
        ("cmd-,", Settings),
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
        ("alt-shift-cmd-escape", ForceQuit),
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
        ("cmd-?", Help),
        ("alt-cmd-'", KeyboardNavigation),
        ("alt-cmd-d", DockHiding),
        ("alt-cmd-8", Zoom),
        ("alt-cmd-=", Zoom),
        ("alt-cmd--", Zoom),
        ("alt-cmd-\\", Zoom),
        ("ctrl-alt-cmd-8", InvertColors),
        ("ctrl-alt-cmd-,", Contrast),
        ("ctrl-alt-cmd-.", Contrast),
        ("cmd-f5", VoiceOver),
        ("alt-cmd-f5", AccessibilityShortcuts),
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
    fn settings_keeps_its_standard_shortcut_and_no_command_can_take_it() {
        let profile = crate::desktop_profile::default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            shortcuts(),
        )
        .unwrap();
        let settings = Shortcut::parse("cmd-,").unwrap();

        assert_eq!(
            profile.check(&settings),
            Err(Reservation::System(SystemReservation::Settings))
        );
        let mut preferences = KeybindingPreferences::default();
        assert_eq!(
            profile.assign(&mut preferences, Command::NewWorkspace, Some(settings)),
            Err(Reservation::System(SystemReservation::Settings))
        );
    }

    #[test]
    fn system_reservations_allow_the_complete_default_profile() {
        let profile = crate::desktop_profile::default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            shortcuts(),
        )
        .unwrap();
        let resolved = profile.resolve(&KeybindingPreferences::default());
        let unassigned = [
            Command::CloseWorkspace,
            Command::MoveTabRight,
            Command::MoveTabLeft,
            Command::KeyboardShortcuts,
        ];
        for command in Command::ALL {
            assert_eq!(
                resolved.shortcut(command).is_none(),
                unassigned.contains(&command),
                "{command:?}"
            );
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

    #[test]
    fn native_shifted_symbols_and_hand_edited_overrides_remain_system_reserved() {
        let profile = crate::desktop_profile::default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            shortcuts(),
        )
        .unwrap();
        for (key, physical, reason) in [
            ("#", "3", SystemReservation::Screenshot),
            ("$", "4", SystemReservation::Screenshot),
            ("%", "5", SystemReservation::Screenshot),
            ("^", "6", SystemReservation::Screenshot),
            ("?", "/", SystemReservation::Help),
            ("~", "`", SystemReservation::WindowCycling),
        ] {
            for control in [false, true] {
                if control && !matches!(reason, SystemReservation::Screenshot) {
                    continue;
                }
                let native = gpui::Keystroke {
                    key: key.into(),
                    key_char: None,
                    modifiers: gpui::Modifiers {
                        platform: true,
                        control,
                        ..Default::default()
                    },
                };
                let shortcut = Shortcut::from_keystroke(&native).unwrap();
                assert_eq!(profile.check(&shortcut), Err(Reservation::System(reason)));
                let prefix = if control { "ctrl-cmd" } else { "cmd" };
                for source in [
                    format!("{prefix}-{key}"),
                    format!("shift-{prefix}-{physical}"),
                ] {
                    let mut preferences: KeybindingPreferences =
                        serde_json::from_value(serde_json::json!({"new_workspace": source}))
                            .unwrap();
                    let resolved = profile.resolve(&preferences);
                    assert_eq!(
                        resolved.shortcut(Command::NewWorkspace),
                        Some(&Shortcut::parse("cmd-n").unwrap())
                    );
                    assert_eq!(
                        resolved.inactive_override(Command::NewWorkspace),
                        Some(Reservation::System(reason))
                    );
                    assert_eq!(
                        profile.assign(
                            &mut preferences,
                            Command::NewWorkspace,
                            Some(shortcut.clone())
                        ),
                        Err(Reservation::System(reason))
                    );
                }
            }
        }
    }

    #[test]
    fn shifted_default_alias_dispatches_and_overrides_share_its_identity() {
        let profile = crate::desktop_profile::default_keymap::profile(
            crate::platform::keyboard_layout::testing::us(),
            super::shortcuts(),
        )
        .unwrap();
        let resolved = profile.resolve(&KeybindingPreferences::default());
        let native = gpui::Keystroke::parse("cmd-+").unwrap();
        assert!(resolved.key_bindings().iter().any(|binding| {
            binding
                .action()
                .partial_eq(Command::IncreaseTerminalFontSize.action().as_ref())
                && native.should_match(&binding.keystrokes()[0])
        }));
        let mut preferences: KeybindingPreferences =
            serde_json::from_str(r#"{"new_workspace":"shift-cmd-="}"#).unwrap();
        assert_eq!(
            profile
                .resolve(&preferences)
                .owner(&Shortcut::from_keystroke(&native).unwrap()),
            Some(Command::NewWorkspace)
        );
        profile
            .assign(
                &mut preferences,
                Command::CreateTab,
                Some(Shortcut::from_keystroke(&native).unwrap()),
            )
            .unwrap();
        let resolved = profile.resolve(&preferences);
        assert_eq!(resolved.shortcut(Command::NewWorkspace), None);
        assert_eq!(
            resolved.owner(&Shortcut::parse("cmd-+").unwrap()),
            Some(Command::CreateTab)
        );
        assert!(resolved.key_bindings().iter().any(|binding| {
            binding
                .action()
                .partial_eq(Command::CreateTab.action().as_ref())
                && native.should_match(&binding.keystrokes()[0])
        }));
        assert!(
            serde_json::from_str::<KeybindingPreferences>(
                r#"{"new_workspace":"shift-cmd-=","create_tab":"cmd-+"}"#
            )
            .unwrap()
            .validate()
            .is_ok()
        );
    }
}
