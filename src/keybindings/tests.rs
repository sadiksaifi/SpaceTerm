use std::collections::HashSet;

use gpui::KeyBinding;

use super::*;

fn shortcut(source: &str) -> Shortcut {
    Shortcut::parse(source).unwrap()
}

fn preferences(source: &str) -> KeybindingPreferences {
    serde_json::from_str(source).unwrap()
}

fn profile() -> KeymapProfile {
    KeymapProfile::new(
        [
            (
                Command::NewWorkspace,
                Some(DefaultBinding::new("cmd-n", &[])),
            ),
            (Command::CloseWorkspace, None),
            (Command::CreateTab, Some(DefaultBinding::new("cmd-t", &[]))),
            (
                Command::CloseTab,
                Some(DefaultBinding::new("shift-cmd-w", &[])),
            ),
            (
                Command::ActivateWorkspace1,
                Some(DefaultBinding::new("ctrl-1", &[])),
            ),
            (Command::SplitRight, Some(DefaultBinding::new("cmd-d", &[]))),
            (
                Command::OpenTerminalFind,
                Some(DefaultBinding::new("cmd-f", &[])),
            ),
            (Command::FindNext, Some(DefaultBinding::new("cmd-g", &[]))),
            (
                Command::FindPrevious,
                Some(DefaultBinding::new("shift-cmd-g", &[])),
            ),
            (
                Command::IncreaseTerminalFontSize,
                Some(DefaultBinding::new("cmd-=", &["cmd-+", "alt-cmd-="])),
            ),
        ],
        vec![SystemReserved {
            shortcut: shortcut("cmd-q"),
            reason: SystemReservation::Quit,
        }],
        vec![KeyBinding::new("cmd-q", crate::app::QuitApplication, None)],
        vec![KeyBinding::new(
            "escape",
            crate::ui::CloseTerminalFind,
            Some(crate::ui::TERMINAL_FIND_KEY_CONTEXT),
        )],
    )
    .unwrap()
}

#[test]
fn command_ids_round_trip_and_groups_are_contiguous_in_settings_order() {
    let mut ids = HashSet::new();
    let mut actions = HashSet::new();
    let mut completed_groups = HashSet::new();
    let mut previous_group = Command::ALL[0].group();
    for command in Command::ALL {
        assert!(ids.insert(command.id()));
        assert!(actions.insert(command.action().name()));
        assert_eq!(Command::from_id(command.id()), Some(command));
        assert_eq!(
            serde_json::from_str::<Command>(&serde_json::to_string(&command).unwrap()).unwrap(),
            command
        );
        assert!(!command.label().is_empty());
        if command.group() != previous_group {
            completed_groups.insert(previous_group);
            assert!(!completed_groups.contains(&command.group()));
            previous_group = command.group();
        }
    }
    assert_eq!(Command::ALL.len(), 43);
    assert_eq!(Command::NewWorkspace.id(), "new_workspace");
    assert_eq!(Command::CloseTab.id(), "close_tab");
    assert_eq!(Command::from_id("NewWorkspace"), None);
    assert!(serde_json::from_str::<Command>("\"unknown\"").is_err());
    assert_eq!(Command::NewWorkspace.label(), "New Workspace");
    assert_eq!(Command::ActivateWorkspace1.label(), "Workspace 1");
    assert_eq!(Command::ActivateTab1.label(), "Tab 1");
    assert_eq!(Command::OpenTerminalFind.label(), "Find");
    assert_eq!(
        Command::ClearTerminalScreenAndScrollback.label(),
        "Clear Screen and Scrollback"
    );
    assert_eq!(
        Command::IncreaseTerminalFontSize.label(),
        "Increase Font Size"
    );
}

#[test]
fn command_scopes_and_actions_preserve_the_existing_binding_contract() {
    let application_commands = [
        Command::SwitchWorkspace,
        Command::NewWorkspace,
        Command::NewRemoteWorkspace,
        Command::CloseWorkspace,
        Command::CreateTab,
        Command::ClosePane,
        Command::CloseTab,
    ];
    for command in Command::ALL {
        let application = application_commands.contains(&command);
        assert_eq!(
            command.scope(),
            if application {
                KeyScope::Application
            } else {
                KeyScope::Workspace
            }
        );
        assert_eq!(
            command.key_context(),
            if application {
                None
            } else {
                Some(crate::ui::TERMINAL_KEY_CONTEXT)
            }
        );
    }
    // The retained baseline checks every already-bound command against its actual action and scope.
    for line in include_str!("../keybindings_baseline.txt").lines() {
        let columns: Vec<_> = line.split('\t').collect();
        let Some(command) = Command::ALL
            .into_iter()
            .find(|command| command.action().name() == columns[2])
        else {
            continue;
        };
        if columns[1].contains(crate::ui::TERMINAL_FIND_KEY_CONTEXT) {
            continue;
        }
        assert_eq!(
            columns[1] == "None",
            command.scope() == KeyScope::Application,
            "{command:?}"
        );
    }
}

#[test]
fn preferences_are_sparse_and_validate_only_override_conflicts() {
    let prefs = preferences(r#"{"close_tab":null,"new_workspace":"CMD-SHIFT-T"}"#);
    assert_eq!(prefs.get(Command::CloseTab), Some(&None));
    assert_eq!(
        prefs.get(Command::NewWorkspace),
        Some(&Some(shortcut("shift-cmd-t")))
    );
    assert!(!prefs.is_overridden(Command::CreateTab));
    assert_eq!(prefs.iter().count(), 2);
    assert_eq!(prefs.validate(), Ok(()));
    assert_eq!(
        serde_json::to_value(&prefs).unwrap(),
        serde_json::json!({"close_tab": null, "new_workspace": "shift-cmd-t"})
    );
    assert_eq!(
        preferences(r#"{"close_tab":"cmd-t","new_workspace":"CMD-T"}"#).validate(),
        Err(KeybindingPreferencesError::DuplicateShortcut)
    );
    assert!(serde_json::from_str::<KeybindingPreferences>(r#"{"unknown":null}"#).is_err());
    assert!(serde_json::from_str::<KeybindingPreferences>(r#"{"close_tab":"ctrl-c"}"#).is_err());
    assert!(serde_json::from_str::<KeybindingPreferences>(r#"{"close_tab":12}"#).is_err());
    assert_eq!(preferences(r#"{"close_tab":"cmd-q"}"#).validate(), Ok(()));
    assert_eq!(
        serde_json::to_string(&KeybindingPreferences::default()).unwrap(),
        "{}"
    );
}

#[test]
fn profile_rejects_invalid_duplicate_and_system_reserved_defaults() {
    let build = |defaults| {
        KeymapProfile::new(
            defaults,
            vec![SystemReserved {
                shortcut: shortcut("cmd-q"),
                reason: SystemReservation::Quit,
            }],
            vec![],
            vec![],
        )
    };
    for (key, expected) in [
        (
            "cmd-unknown",
            KeymapProfileError::InvalidDefault(ShortcutRejection::UnsupportedKey),
        ),
        (
            "ctrl-c",
            KeymapProfileError::InvalidDefault(ShortcutRejection::TerminalReserved(
                TerminalConvention::ControlCharacter,
            )),
        ),
        (
            "cmd-q",
            KeymapProfileError::SystemReserved(SystemReservation::Quit),
        ),
    ] {
        for default in [
            DefaultBinding::new(key, &[]),
            DefaultBinding::new("cmd-n", &[key]),
        ] {
            assert_eq!(
                build(vec![(Command::NewWorkspace, Some(default))]).unwrap_err(),
                expected
            );
        }
    }
    assert_eq!(
        build(vec![
            (Command::NewWorkspace, None),
            (Command::NewWorkspace, None)
        ])
        .unwrap_err(),
        KeymapProfileError::DuplicateCommand
    );
    for defaults in [
        vec![(
            Command::NewWorkspace,
            Some(DefaultBinding::new("cmd-n", &["CMD-N"])),
        )],
        vec![
            (
                Command::NewWorkspace,
                Some(DefaultBinding::new("cmd-n", &[])),
            ),
            (Command::SplitRight, Some(DefaultBinding::new("cmd-n", &[]))),
        ],
        vec![
            (
                Command::NewWorkspace,
                Some(DefaultBinding::new("cmd-n", &["cmd-="])),
            ),
            (Command::SplitRight, Some(DefaultBinding::new("cmd-=", &[]))),
        ],
    ] {
        assert_eq!(
            build(defaults).unwrap_err(),
            KeymapProfileError::DuplicateShortcut
        );
    }
    let reserved = SystemReserved {
        shortcut: shortcut("cmd-q"),
        reason: SystemReservation::Quit,
    };
    assert_eq!(
        KeymapProfile::new([], vec![reserved.clone(), reserved], vec![], vec![]).unwrap_err(),
        KeymapProfileError::DuplicateSystemReservation
    );
}

#[test]
fn resolve_defaults_preserves_primary_alias_order_and_separates_fixed_controls() {
    let profile = profile();
    let resolved = profile.resolve(&KeybindingPreferences::default());
    assert_eq!(
        resolved.state(Command::NewWorkspace),
        KeybindingState::Default
    );
    assert_eq!(
        resolved.shortcut(Command::NewWorkspace),
        Some(&shortcut("cmd-n"))
    );
    assert_eq!(
        resolved.shortcuts(Command::IncreaseTerminalFontSize),
        &[shortcut("cmd-="), shortcut("cmd-+"), shortcut("alt-cmd-=")]
    );
    assert_eq!(
        resolved.state(Command::CloseWorkspace),
        KeybindingState::Unassigned
    );
    assert_eq!(resolved.shortcut(Command::CloseWorkspace), None);
    assert_eq!(
        resolved.owner(&shortcut("cmd-+")),
        Some(Command::IncreaseTerminalFontSize)
    );
    assert_eq!(resolved.owner(&shortcut("cmd-q")), None);
    assert_eq!(profile.fixed_bindings().len(), 1);
    assert_eq!(
        profile.fixed_bindings()[0].keystrokes()[0].unparse(),
        "cmd-q"
    );
    assert_eq!(profile.control_bindings().len(), 1);
    assert_eq!(
        profile.control_bindings()[0].keystrokes()[0].unparse(),
        "escape"
    );
    assert_eq!(profile.fixed_bindings()[0].meta(), None);
    assert_eq!(profile.control_bindings()[0].meta(), None);
}

#[test]
fn resolve_overrides_displace_default_primaries_and_drop_claimed_aliases() {
    let profile = profile();
    let prefs = preferences(
        r#"{"new_workspace":"ctrl-1","close_tab":null,"split_right":"cmd-+","create_tab":"cmd-q"}"#,
    );
    let resolved = profile.resolve(&prefs);
    assert_eq!(
        resolved.state(Command::NewWorkspace),
        KeybindingState::Overridden
    );
    assert_eq!(
        resolved.shortcut(Command::NewWorkspace),
        Some(&shortcut("ctrl-1"))
    );
    assert_eq!(
        resolved.state(Command::ActivateWorkspace1),
        KeybindingState::Displaced {
            by: Command::NewWorkspace
        }
    );
    assert!(resolved.shortcuts(Command::ActivateWorkspace1).is_empty());
    assert_eq!(
        resolved.state(Command::CloseTab),
        KeybindingState::Unassigned
    );
    assert_eq!(
        resolved.state(Command::CreateTab),
        KeybindingState::Blocked(SystemReservation::Quit)
    );
    assert!(resolved.shortcuts(Command::CreateTab).is_empty());
    assert_eq!(
        resolved.state(Command::IncreaseTerminalFontSize),
        KeybindingState::Default
    );
    assert_eq!(
        resolved.shortcuts(Command::IncreaseTerminalFontSize),
        &[shortcut("cmd-="), shortcut("alt-cmd-=")]
    );
    assert_eq!(resolved.owner(&shortcut("cmd-n")), None);
    assert_eq!(resolved.owner(&shortcut("cmd-t")), None);
    assert_eq!(profile.resolve(&prefs), resolved);
}

#[test]
fn a_displaced_primary_disables_all_its_aliases() {
    let resolved = profile().resolve(&preferences(r#"{"new_workspace":"cmd-="}"#));
    assert_eq!(
        resolved.state(Command::IncreaseTerminalFontSize),
        KeybindingState::Displaced {
            by: Command::NewWorkspace
        }
    );
    assert!(
        resolved
            .shortcuts(Command::IncreaseTerminalFontSize)
            .is_empty()
    );
    assert_eq!(resolved.owner(&shortcut("cmd-+")), None);
}

#[test]
fn every_system_reason_blocks_hand_edits_and_rejects_assign_without_mutation() {
    for reason in [
        SystemReservation::Copy,
        SystemReservation::Paste,
        SystemReservation::Cut,
        SystemReservation::Undo,
        SystemReservation::Redo,
        SystemReservation::SelectAll,
        SystemReservation::Quit,
        SystemReservation::Hide,
        SystemReservation::HideOthers,
        SystemReservation::Minimize,
        SystemReservation::MinimizeAll,
        SystemReservation::FullScreen,
        SystemReservation::AppSwitcher,
        SystemReservation::WindowCycling,
        SystemReservation::Spotlight,
        SystemReservation::CharacterViewer,
        SystemReservation::ForceQuit,
        SystemReservation::LockScreen,
        SystemReservation::LogOut,
        SystemReservation::Screenshot,
        SystemReservation::Help,
    ] {
        let profile = KeymapProfile::new(
            [],
            vec![SystemReserved {
                shortcut: shortcut("cmd-q"),
                reason,
            }],
            vec![],
            vec![],
        )
        .unwrap();
        let mut prefs = preferences(r#"{"new_workspace":"cmd-q"}"#);
        let before = prefs.clone();
        assert_eq!(
            profile.resolve(&prefs).state(Command::NewWorkspace),
            KeybindingState::Blocked(reason)
        );
        assert_eq!(
            profile.check(&shortcut("cmd-q")),
            Err(Reservation::System(reason))
        );
        assert_eq!(
            profile.assign(&mut prefs, Command::CreateTab, Some(shortcut("cmd-q"))),
            Err(Reservation::System(reason))
        );
        assert_eq!(prefs, before);
    }
    assert_eq!(profile().check(&shortcut("ctrl-1")), Ok(()));
}

#[test]
fn assigning_a_primary_clears_its_previous_owner_across_scopes() {
    let profile = profile();
    let mut prefs = KeybindingPreferences::default();
    let result = profile
        .assign(&mut prefs, Command::NewWorkspace, Some(shortcut("ctrl-1")))
        .unwrap();
    assert_eq!(result.displaced, Some(Command::ActivateWorkspace1));
    assert_eq!(prefs.get(Command::ActivateWorkspace1), Some(&None));
    profile
        .assign(&mut prefs, Command::NewWorkspace, None)
        .unwrap();
    assert_eq!(
        profile.resolve(&prefs).state(Command::ActivateWorkspace1),
        KeybindingState::Unassigned
    );
    assert_eq!(profile.resolve(&prefs).owner(&shortcut("ctrl-1")), None);
}

#[test]
fn assigning_an_alias_pins_the_primary_and_drops_all_aliases() {
    let profile = profile();
    let mut prefs = KeybindingPreferences::default();
    assert_eq!(
        profile
            .assign(&mut prefs, Command::NewWorkspace, Some(shortcut("cmd-+")))
            .unwrap()
            .displaced,
        Some(Command::IncreaseTerminalFontSize)
    );
    assert_eq!(
        prefs.get(Command::IncreaseTerminalFontSize),
        Some(&Some(shortcut("cmd-=")))
    );
    let resolved = profile.resolve(&prefs);
    assert_eq!(
        resolved.state(Command::IncreaseTerminalFontSize),
        KeybindingState::Overridden
    );
    assert_eq!(
        resolved.shortcuts(Command::IncreaseTerminalFontSize),
        &[shortcut("cmd-=")]
    );
    assert_eq!(resolved.owner(&shortcut("alt-cmd-=")), None);
    profile
        .assign(&mut prefs, Command::NewWorkspace, None)
        .unwrap();
    assert_eq!(profile.resolve(&prefs).owner(&shortcut("cmd-+")), None);
}

#[test]
fn assigning_defaults_normalizes_only_alias_free_defaults() {
    let profile = profile();
    let mut prefs = preferences(r#"{"new_workspace":"cmd-y"}"#);
    assert_eq!(
        profile
            .assign(&mut prefs, Command::NewWorkspace, Some(shortcut("cmd-n")))
            .unwrap()
            .displaced,
        None
    );
    assert!(!prefs.is_overridden(Command::NewWorkspace));
    profile
        .assign(
            &mut prefs,
            Command::IncreaseTerminalFontSize,
            Some(shortcut("cmd-=")),
        )
        .unwrap();
    assert!(prefs.is_overridden(Command::IncreaseTerminalFontSize));
    assert_eq!(
        profile
            .resolve(&prefs)
            .shortcuts(Command::IncreaseTerminalFontSize),
        &[shortcut("cmd-=")]
    );
    profile
        .assign(&mut prefs, Command::CloseWorkspace, None)
        .unwrap();
    assert!(!prefs.is_overridden(Command::CloseWorkspace));
    profile
        .assign(&mut prefs, Command::CloseWorkspace, Some(shortcut("cmd-y")))
        .unwrap();
    profile.reset(&mut prefs, Command::CloseWorkspace);
    assert!(!prefs.is_overridden(Command::CloseWorkspace));
}

#[test]
fn assigning_an_owned_shortcut_keeps_the_command_assigned() {
    let profile = profile();
    let mut prefs = KeybindingPreferences::default();
    for source in ["cmd-+", "cmd-+", "cmd-="] {
        assert_eq!(
            profile
                .assign(
                    &mut prefs,
                    Command::IncreaseTerminalFontSize,
                    Some(shortcut(source))
                )
                .unwrap()
                .displaced,
            None
        );
        assert_eq!(
            profile
                .resolve(&prefs)
                .shortcut(Command::IncreaseTerminalFontSize),
            Some(&shortcut(source))
        );
    }
}

#[test]
fn reset_reclaims_the_primary_and_every_alias_from_current_owners() {
    let profile = profile();
    let mut prefs = preferences(
        r#"{"increase_terminal_font_size":null,"new_workspace":"cmd-=","create_tab":"cmd-+","close_tab":"alt-cmd-="}"#,
    );
    profile.reset(&mut prefs, Command::IncreaseTerminalFontSize);
    assert!(!prefs.is_overridden(Command::IncreaseTerminalFontSize));
    for command in [Command::NewWorkspace, Command::CreateTab, Command::CloseTab] {
        assert_eq!(prefs.get(command), Some(&None));
    }
    let resolved = profile.resolve(&prefs);
    assert_eq!(
        resolved.shortcuts(Command::IncreaseTerminalFontSize),
        &[shortcut("cmd-="), shortcut("cmd-+"), shortcut("alt-cmd-=")]
    );
    assert_eq!(prefs.validate(), Ok(()));
}

#[test]
fn reset_reclaims_a_default_even_without_an_override_on_the_reset_command() {
    let profile = profile();
    let mut prefs = preferences(r#"{"split_right":"cmd-n"}"#);
    profile.reset(&mut prefs, Command::NewWorkspace);
    assert_eq!(prefs.get(Command::SplitRight), Some(&None));
    assert_eq!(
        profile.resolve(&prefs).state(Command::NewWorkspace),
        KeybindingState::Default
    );
}

#[test]
fn a_displacement_that_restores_a_default_retains_no_override() {
    let profile = profile();
    let mut prefs = preferences(r#"{"close_workspace":"cmd-n"}"#);
    profile.reset(&mut prefs, Command::NewWorkspace);
    assert_eq!(prefs.get(Command::CloseWorkspace), None);
    let mut prefs = preferences(r#"{"close_workspace":"cmd-y"}"#);
    let displaced = profile
        .assign(&mut prefs, Command::CreateTab, Some(shortcut("cmd-y")))
        .unwrap()
        .displaced;
    assert_eq!(displaced, Some(Command::CloseWorkspace));
    assert_eq!(prefs.get(Command::CloseWorkspace), None);
}

#[test]
fn bindings_follow_command_order_and_mirror_each_find_shortcut_immediately() {
    let resolved = profile().resolve(&preferences(
        r#"{"find_next":"cmd-y","find_previous":null}"#,
    ));
    let actual: Vec<_> = resolved
        .key_bindings()
        .into_iter()
        .map(|binding| {
            assert_eq!(binding.meta(), Some(CUSTOMIZABLE_BINDINGS));
            assert_eq!(binding.keystrokes().len(), 1);
            (
                binding.action().name(),
                binding.keystrokes()[0].unparse(),
                binding.predicate().map(|predicate| predicate.to_string()),
            )
        })
        .collect();
    let expected = [
        (Command::NewWorkspace, "cmd-n", None),
        (
            Command::ActivateWorkspace1,
            "ctrl-1",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (Command::CreateTab, "cmd-t", None),
        (Command::CloseTab, "cmd-shift-w", None),
        (
            Command::SplitRight,
            "cmd-d",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (
            Command::OpenTerminalFind,
            "cmd-f",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (
            Command::OpenTerminalFind,
            "cmd-f",
            Some(crate::ui::TERMINAL_FIND_KEY_CONTEXT),
        ),
        (
            Command::FindNext,
            "cmd-y",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (
            Command::FindNext,
            "cmd-y",
            Some(crate::ui::TERMINAL_FIND_KEY_CONTEXT),
        ),
        (
            Command::IncreaseTerminalFontSize,
            "cmd-=",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (
            Command::IncreaseTerminalFontSize,
            "cmd-+",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
        (
            Command::IncreaseTerminalFontSize,
            "alt-cmd-=",
            Some(crate::ui::TERMINAL_KEY_CONTEXT),
        ),
    ]
    .map(|(command, keys, context)| {
        (
            command.action().name(),
            keys.to_owned(),
            context.map(str::to_owned),
        )
    });
    assert_eq!(actual, expected);
}

#[test]
fn deterministic_assign_reset_clear_sequence_preserves_unique_ownership() {
    let profile = profile();
    let mut prefs = KeybindingPreferences::default();
    let keys = [
        "cmd-n",
        "cmd-t",
        "shift-cmd-w",
        "ctrl-1",
        "cmd-d",
        "cmd-f",
        "cmd-g",
        "shift-cmd-g",
        "cmd-=",
        "cmd-+",
        "alt-cmd-=",
        "cmd-y",
        "cmd-q",
    ];
    let mut seed = 0xa53c_9e71_u64;
    for _ in 0..4096 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let command = Command::ALL[(seed >> 32) as usize % Command::ALL.len()];
        match seed % 4 {
            0 => profile.reset(&mut prefs, command),
            1 => {
                profile.assign(&mut prefs, command, None).unwrap();
            }
            _ => {
                let source = keys[(seed >> 16) as usize % keys.len()];
                let before = prefs.clone();
                if profile
                    .assign(&mut prefs, command, Some(shortcut(source)))
                    .is_err()
                {
                    assert_eq!(prefs, before);
                }
            }
        }
        assert_eq!(prefs.validate(), Ok(()));
        let resolved = profile.resolve(&prefs);
        let mut seen = HashSet::new();
        for command in Command::ALL {
            for shortcut in resolved.shortcuts(command) {
                assert!(seen.insert(shortcut.clone()));
                assert_eq!(resolved.owner(shortcut), Some(command));
                assert_eq!(profile.check(shortcut), Ok(()));
            }
        }
        for source in keys {
            let key = shortcut(source);
            assert_eq!(seen.contains(&key), resolved.owner(&key).is_some());
        }
        assert_eq!(
            serde_json::from_str::<KeybindingPreferences>(&serde_json::to_string(&prefs).unwrap())
                .unwrap(),
            prefs
        );
    }
}
