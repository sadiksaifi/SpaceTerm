//! The Linux application Shortcut policy: Commands take Ctrl+Shift, as in VTE terminals, plus the
//! few chords those terminals also give the application, such as Alt+digit for Tabs, Ctrl+Page Up
//! and Ctrl+Page Down, and Shift with the paging keys. Plain Ctrl+letter and Alt+letter reach
//! programs in the terminal, and Super stays with the desktop.
use std::rc::Rc;

use gpui::KeyBinding;
use spaceterm_ui::{EditCopy, EditPaste};

#[cfg(test)]
use super::keyboard_layout::KeyboardLayout;
use super::keyboard_layout::KeyboardLayoutAdapter;
use crate::app::*;
use crate::keybindings::{
    Command, DefaultBinding, KeymapProfile, KeymapProfileError, SystemReserved, TerminalConventions,
};
use crate::ui::*;

#[cfg(feature = "developer-tools")]
const OPEN_DEVELOPER_WORKBENCH: &str = "ctrl-shift-alt-a";
#[cfg(feature = "developer-tools")]
const TOGGLE_APPEARANCE_PREVIEW: &str = "ctrl-shift-alt-c";

pub(super) fn profile(
    layout: Rc<dyn KeyboardLayoutAdapter>,
    system_reserved: Vec<SystemReserved>,
) -> Result<KeymapProfile, KeymapProfileError> {
    #[cfg(feature = "developer-tools")]
    let system_reserved = system_reserved
        .into_iter()
        .chain(developer_reservations())
        .collect();
    let defaults = [
        (
            Command::SwitchWorkspace,
            Some(DefaultBinding::new("ctrl-shift-k", &[])),
        ),
        (
            Command::NewWorkspace,
            Some(DefaultBinding::new("ctrl-shift-n", &[])),
        ),
        (
            Command::NewRemoteWorkspace,
            Some(DefaultBinding::new("ctrl-shift-alt-n", &[])),
        ),
        (
            Command::OpenLocalDirectory,
            Some(DefaultBinding::new("ctrl-shift-o", &[])),
        ),
        (
            Command::OpenRemoteDirectory,
            Some(DefaultBinding::new("ctrl-shift-alt-o", &[])),
        ),
        (Command::CloseWorkspace, None),
        (
            Command::ActivateWorkspace1,
            Some(DefaultBinding::new("ctrl-alt-1", &[])),
        ),
        (
            Command::ActivateWorkspace2,
            Some(DefaultBinding::new("ctrl-alt-2", &[])),
        ),
        (
            Command::ActivateWorkspace3,
            Some(DefaultBinding::new("ctrl-alt-3", &[])),
        ),
        (
            Command::ActivateWorkspace4,
            Some(DefaultBinding::new("ctrl-alt-4", &[])),
        ),
        (
            Command::ActivateWorkspace5,
            Some(DefaultBinding::new("ctrl-alt-5", &[])),
        ),
        (
            Command::ActivateWorkspace6,
            Some(DefaultBinding::new("ctrl-alt-6", &[])),
        ),
        (
            Command::ActivateWorkspace7,
            Some(DefaultBinding::new("ctrl-alt-7", &[])),
        ),
        (
            Command::ActivateWorkspace8,
            Some(DefaultBinding::new("ctrl-alt-8", &[])),
        ),
        (
            Command::ActivateWorkspace9,
            Some(DefaultBinding::new("ctrl-alt-9", &[])),
        ),
        (
            Command::CreateTab,
            Some(DefaultBinding::new("ctrl-shift-t", &[])),
        ),
        (
            Command::CloseTab,
            Some(DefaultBinding::new("ctrl-shift-alt-w", &[])),
        ),
        (
            Command::ActivateTab1,
            Some(DefaultBinding::new("alt-1", &[])),
        ),
        (
            Command::ActivateTab2,
            Some(DefaultBinding::new("alt-2", &[])),
        ),
        (
            Command::ActivateTab3,
            Some(DefaultBinding::new("alt-3", &[])),
        ),
        (
            Command::ActivateTab4,
            Some(DefaultBinding::new("alt-4", &[])),
        ),
        (
            Command::ActivateTab5,
            Some(DefaultBinding::new("alt-5", &[])),
        ),
        (
            Command::ActivateTab6,
            Some(DefaultBinding::new("alt-6", &[])),
        ),
        (
            Command::ActivateTab7,
            Some(DefaultBinding::new("alt-7", &[])),
        ),
        (
            Command::ActivateTab8,
            Some(DefaultBinding::new("alt-8", &[])),
        ),
        (
            Command::ActivateTab9,
            Some(DefaultBinding::new("alt-9", &[])),
        ),
        (
            Command::NextTab,
            Some(DefaultBinding::new("ctrl-pagedown", &["ctrl-tab"])),
        ),
        (
            Command::PreviousTab,
            Some(DefaultBinding::new("ctrl-pageup", &["ctrl-shift-tab"])),
        ),
        (
            Command::MoveTabRight,
            Some(DefaultBinding::new("ctrl-shift-pagedown", &[])),
        ),
        (
            Command::MoveTabLeft,
            Some(DefaultBinding::new("ctrl-shift-pageup", &[])),
        ),
        (
            Command::ClosePane,
            Some(DefaultBinding::new("ctrl-shift-w", &[])),
        ),
        (
            Command::SplitRight,
            Some(DefaultBinding::new("ctrl-shift-d", &[])),
        ),
        (
            Command::SplitDown,
            Some(DefaultBinding::new("ctrl-shift-alt-d", &[])),
        ),
        (
            Command::FocusPaneLeft,
            Some(DefaultBinding::new("ctrl-shift-left", &[])),
        ),
        (
            Command::FocusPaneRight,
            Some(DefaultBinding::new("ctrl-shift-right", &[])),
        ),
        (
            Command::FocusPaneUp,
            Some(DefaultBinding::new("ctrl-shift-up", &[])),
        ),
        (
            Command::FocusPaneDown,
            Some(DefaultBinding::new("ctrl-shift-down", &[])),
        ),
        (
            Command::FocusPreviousPane,
            Some(DefaultBinding::new("ctrl-shift-[", &["ctrl-shift-alt-f6"])),
        ),
        (
            Command::FocusNextPane,
            Some(DefaultBinding::new("ctrl-shift-]", &["ctrl-shift-f6"])),
        ),
        (
            Command::TogglePaneZoom,
            Some(DefaultBinding::new("ctrl-shift-enter", &[])),
        ),
        (
            Command::OpenTerminalFind,
            Some(DefaultBinding::new("ctrl-shift-f", &[])),
        ),
        (
            Command::FindNext,
            Some(DefaultBinding::new("ctrl-shift-g", &["f3"])),
        ),
        (
            Command::FindPrevious,
            Some(DefaultBinding::new("ctrl-shift-h", &["shift-f3"])),
        ),
        (
            Command::ClearTerminalScreenAndScrollback,
            Some(DefaultBinding::new("ctrl-shift-alt-k", &[])),
        ),
        (
            Command::ScrollPageUp,
            Some(DefaultBinding::new("shift-pageup", &[])),
        ),
        (
            Command::ScrollPageDown,
            Some(DefaultBinding::new("shift-pagedown", &[])),
        ),
        (
            Command::ScrollToTop,
            Some(DefaultBinding::new("shift-home", &[])),
        ),
        (
            Command::ScrollToBottom,
            Some(DefaultBinding::new("shift-end", &[])),
        ),
        (
            Command::IncreaseTerminalFontSize,
            Some(DefaultBinding::new("ctrl-=", &["ctrl-+"])),
        ),
        (
            Command::DecreaseTerminalFontSize,
            Some(DefaultBinding::new("ctrl--", &[])),
        ),
        (
            Command::ResetTerminalFontSize,
            Some(DefaultBinding::new("ctrl-0", &[])),
        ),
        (
            Command::ToggleSidebar,
            Some(DefaultBinding::new("ctrl-shift-b", &[])),
        ),
        (
            Command::ToggleSidebarFocus,
            Some(DefaultBinding::new("ctrl-shift-e", &[])),
        ),
        (
            Command::KeyboardShortcuts,
            Some(DefaultBinding::new("ctrl-?", &[])),
        ),
    ];
    KeymapProfile::new(
        layout,
        TerminalConventions::ControlShiftShortcuts,
        defaults,
        system_reserved,
        fixed_bindings(),
        control_bindings(),
    )
}

fn fixed_bindings() -> Vec<KeyBinding> {
    let bindings = vec![
        KeyBinding::new("ctrl-shift-c", EditCopy, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-insert", CopySelection, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-shift-v", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "shift-insert",
            crate::ui::PasteSelection,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new("ctrl-,", crate::ui::settings_window::OpenSettings, None),
        KeyBinding::new("ctrl-shift-q", QuitApplication, None),
        KeyBinding::new("ctrl-shift-m", MinimizeWindow, None),
        KeyBinding::new("f11", ToggleFullScreen, None),
    ];
    #[cfg(feature = "developer-tools")]
    let bindings = bindings
        .into_iter()
        .chain([
            KeyBinding::new(
                OPEN_DEVELOPER_WORKBENCH,
                crate::ui::developer_workbench::OpenDeveloperWorkbench,
                None,
            ),
            KeyBinding::new(
                TOGGLE_APPEARANCE_PREVIEW,
                crate::ui::developer_workbench::ToggleAppearancePreview,
                None,
            ),
        ])
        .collect();
    bindings
}

#[cfg(feature = "developer-tools")]
fn developer_reservations() -> [SystemReserved; 2] {
    use crate::keybindings::{Shortcut, SystemReservation};
    [
        (
            OPEN_DEVELOPER_WORKBENCH,
            SystemReservation::DeveloperWorkbench,
        ),
        (
            TOGGLE_APPEARANCE_PREVIEW,
            SystemReservation::AppearancePreview,
        ),
    ]
    .map(|(source, reason)| SystemReserved {
        shortcut: Shortcut::parse(source).expect("valid static developer shortcut"),
        reason,
    })
}

fn control_bindings() -> Vec<KeyBinding> {
    let settings = Some(crate::ui::settings_window::SETTINGS_KEY_CONTEXT);
    let bindings = vec![
        KeyBinding::new("shift-enter", FindPrevious, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("escape", CloseTerminalFind, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new(
            "tab",
            FocusNextTerminalFindControl,
            Some(TERMINAL_FIND_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "shift-tab",
            FocusPreviousTerminalFindControl,
            Some(TERMINAL_FIND_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "enter",
            ConfirmUnsafePaste,
            Some(TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            CancelUnsafePaste,
            Some(TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT),
        ),
        // The Settings window hosts no terminal, so plain Control keeps its GTK meaning there.
        KeyBinding::new(
            "ctrl-shift-w",
            crate::ui::settings_window::CloseSettingsWindow,
            settings,
        ),
        KeyBinding::new(
            "ctrl-w",
            crate::ui::settings_window::CloseSettingsWindow,
            settings,
        ),
        KeyBinding::new(
            "ctrl-shift-f",
            crate::ui::settings_window::FocusSettingsSearch,
            settings,
        ),
        KeyBinding::new(
            "ctrl-f",
            crate::ui::settings_window::FocusSettingsSearch,
            settings,
        ),
        KeyBinding::new(
            "escape",
            crate::ui::settings_window::ClearSettingsSearch,
            settings,
        ),
    ];
    #[cfg(feature = "developer-tools")]
    let bindings = bindings
        .into_iter()
        .chain([
            KeyBinding::new(
                "ctrl-shift-w",
                crate::ui::developer_workbench::CloseDeveloperWorkbench,
                Some(crate::ui::developer_workbench::WORKBENCH_KEY_CONTEXT),
            ),
            KeyBinding::new(
                "ctrl-w",
                crate::ui::developer_workbench::CloseDeveloperWorkbench,
                Some(crate::ui::developer_workbench::WORKBENCH_KEY_CONTEXT),
            ),
        ])
        .collect();
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keybindings::KeybindingPreferences;

    #[derive(Debug)]
    struct ChangingLayout(std::cell::RefCell<KeyboardLayout>);

    impl KeyboardLayoutAdapter for ChangingLayout {
        fn snapshot(
            &self,
            _: &dyn gpui::PlatformKeyboardLayout,
        ) -> Result<KeyboardLayout, crate::platform::keyboard_layout::KeyboardLayoutUnavailable>
        {
            Ok(self.0.borrow().clone())
        }
    }

    #[test]
    fn linux_settings_shortcut_keeps_its_unshifted_key_on_every_layout() {
        let layout = Rc::new(ChangingLayout(std::cell::RefCell::new(
            KeyboardLayout::us_english(),
        )));
        let mut profile = super::profile(layout.clone(), vec![]).unwrap();
        let mut german = KeyboardLayout::default();
        german.insert(false, ",", ";");
        *layout.0.borrow_mut() = german;
        assert!(
            profile
                .refresh_layout(&crate::platform::keyboard_layout::testing::UnknownLayout)
                .unwrap()
        );
        let settings = profile
            .fixed_bindings()
            .iter()
            .find(|binding| {
                binding
                    .action()
                    .partial_eq(&crate::ui::settings_window::OpenSettings)
            })
            .unwrap();
        // Ctrl+, has no Shift for a layout to consume, unlike the former Ctrl+Shift+,.
        assert_eq!(settings.keystrokes()[0].inner().key, ",");
        assert_eq!(
            settings.keystrokes()[0].inner().modifiers,
            gpui::Modifiers::control()
        );
    }

    #[test]
    fn linux_default_commands_resolve_on_us_german_and_french_layouts() {
        use crate::keybindings::KeybindingState;
        use crate::platform::keyboard_layout::testing;
        for (layout, focus_previous_pane) in [
            (KeyboardLayout::us_english(), "ctrl-{"),
            (testing::de(), "ctrl-shift-["),
            (testing::fr_azerty(), "ctrl-shift-["),
        ] {
            let mut profile = super::profile(
                Rc::new(layout),
                super::super::linux_reserved_shortcuts::shortcuts(),
            )
            .unwrap();
            profile.refresh_layout(&testing::UnknownLayout).unwrap();
            let resolved = profile.resolve(&KeybindingPreferences::default());
            for command in Command::ALL {
                let expected = if command == Command::CloseWorkspace {
                    KeybindingState::Unassigned
                } else {
                    KeybindingState::Default
                };
                assert_eq!(resolved.state(command), expected, "{command:?}");
            }
            // Unshifted digits and plain Ctrl symbols keep their spelling on every layout.
            for (command, expected) in [
                (Command::ActivateWorkspace1, "ctrl-alt-1"),
                (Command::ActivateTab1, "alt-1"),
                (Command::IncreaseTerminalFontSize, "ctrl-="),
                (Command::DecreaseTerminalFontSize, "ctrl--"),
                (Command::ResetTerminalFontSize, "ctrl-0"),
                (Command::FocusPreviousPane, focus_previous_pane),
            ] {
                assert_eq!(
                    resolved.shortcut(command).unwrap().to_string(),
                    expected,
                    "{command:?}"
                );
            }
        }
    }

    fn profile() -> KeymapProfile {
        super::profile(
            Rc::new(super::super::linux_keyboard_layout::LinuxKeyboardLayout),
            super::super::linux_reserved_shortcuts::shortcuts(),
        )
        .unwrap()
    }

    #[test]
    fn linux_defaults_resolve_to_the_chords_gpui_dispatches() {
        let resolved = profile().resolve(&KeybindingPreferences::default());
        for (command, dispatch) in [
            (Command::SwitchWorkspace, &["ctrl-shift-k"][..]),
            (Command::NewRemoteWorkspace, &["ctrl-alt-shift-n"]),
            (Command::CreateTab, &["ctrl-shift-t"]),
            (Command::CloseTab, &["ctrl-alt-shift-w"]),
            (Command::ActivateWorkspace1, &["ctrl-alt-1"]),
            (Command::ActivateWorkspace9, &["ctrl-alt-9"]),
            (Command::ActivateTab1, &["alt-1"]),
            (Command::ActivateTab9, &["alt-9"]),
            (Command::NextTab, &["ctrl-pagedown", "ctrl-tab"]),
            (Command::PreviousTab, &["ctrl-pageup", "ctrl-shift-tab"]),
            (Command::MoveTabRight, &["ctrl-shift-pagedown"]),
            (Command::MoveTabLeft, &["ctrl-shift-pageup"]),
            (Command::FocusPaneLeft, &["ctrl-shift-left"]),
            (Command::FocusPreviousPane, &["ctrl-{", "ctrl-alt-shift-f6"]),
            (Command::FocusNextPane, &["ctrl-}", "ctrl-shift-f6"]),
            (Command::FindNext, &["ctrl-shift-g", "f3"]),
            (Command::FindPrevious, &["ctrl-shift-h", "shift-f3"]),
            (Command::ScrollPageUp, &["shift-pageup"]),
            (Command::ScrollPageDown, &["shift-pagedown"]),
            (Command::ScrollToTop, &["shift-home"]),
            (Command::ScrollToBottom, &["shift-end"]),
            (Command::IncreaseTerminalFontSize, &["ctrl-=", "ctrl-+"]),
            (Command::DecreaseTerminalFontSize, &["ctrl--"]),
            (Command::ResetTerminalFontSize, &["ctrl-0"]),
            (Command::ToggleSidebar, &["ctrl-shift-b"]),
            (Command::KeyboardShortcuts, &["ctrl-?"]),
        ] {
            assert_eq!(
                resolved
                    .shortcuts(command)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                dispatch,
                "{command:?}"
            );
        }
        assert_eq!(resolved.shortcut(Command::CloseWorkspace), None);
    }

    #[test]
    fn an_override_reserved_on_this_host_keeps_the_host_default_in_both_directions() {
        use crate::keybindings::{KeybindingState, Reservation, SystemReservation};
        let cases = [
            (
                profile(),
                r#"{"create_tab":"cmd-t","split_right":"cmd-d"}"#,
                [
                    (Command::CreateTab, "ctrl-shift-t"),
                    (Command::SplitRight, "ctrl-shift-d"),
                ],
                Reservation::System(SystemReservation::DesktopShortcut),
            ),
            (
                crate::desktop_profile::default_keymap::profile(
                    crate::platform::keyboard_layout::testing::us(),
                    vec![],
                )
                .unwrap(),
                r#"{"create_tab":"ctrl-shift-t","split_right":"ctrl-shift-d"}"#,
                [
                    (Command::CreateTab, "cmd-t"),
                    (Command::SplitRight, "cmd-d"),
                ],
                Reservation::Terminal(crate::keybindings::TerminalConvention::ControlCharacter),
            ),
        ];
        for (host, overrides, defaults, reservation) in cases {
            let preferences: KeybindingPreferences = serde_json::from_str(overrides).unwrap();
            let retained = serde_json::to_string(&preferences).unwrap();
            let resolved = host.resolve(&preferences);
            for (command, default) in defaults {
                assert_eq!(resolved.state(command), KeybindingState::Default);
                assert_eq!(
                    resolved
                        .shortcut(command)
                        .map(ToString::to_string)
                        .as_deref(),
                    Some(default),
                    "{command:?}"
                );
                assert_eq!(resolved.inactive_override(command), Some(reservation));
                assert!(resolved.key_bindings().iter().any(|binding| {
                    binding.action().partial_eq(command.action().as_ref())
                        && binding.keystrokes()[0].inner().unparse()
                            == gpui::Keystroke::parse(default).unwrap().unparse()
                }));
            }
            assert_eq!(resolved.inactive_override(Command::NewWorkspace), None);
            // Resolution never rewrites the retained overrides.
            assert_eq!(serde_json::to_string(&preferences).unwrap(), retained);
        }
    }

    #[gpui::test]
    fn scroll_shortcuts_from_sidebar_focus_use_the_host_keymap(cx: &mut gpui::TestAppContext) {
        crate::ui::assert_scroll_shortcuts_from_sidebar_focus(profile(), cx);
    }

    #[cfg(feature = "developer-tools")]
    #[test]
    fn workbench_close_shortcuts_do_not_capture_terminal_control_w() {
        let bindings = control_bindings();
        let workbench =
            gpui::KeyContext::parse(crate::ui::developer_workbench::WORKBENCH_KEY_CONTEXT).unwrap();
        let terminal = gpui::KeyContext::parse(TERMINAL_KEY_CONTEXT).unwrap();
        for shortcut in ["ctrl-w", "ctrl-shift-w"] {
            let key = gpui::Keystroke::parse(shortcut).unwrap();
            let binding = bindings
                .iter()
                .find(|binding| {
                    binding
                        .action()
                        .as_any()
                        .is::<crate::ui::developer_workbench::CloseDeveloperWorkbench>()
                        && binding.match_keystrokes(std::slice::from_ref(&key)) == Some(false)
                })
                .expect("the Workbench has a close shortcut");
            let predicate = binding.predicate().unwrap();
            assert!(predicate.eval(std::slice::from_ref(&workbench)));
            assert!(!predicate.eval(std::slice::from_ref(&terminal)));
        }
    }

    #[test]
    fn linux_fixed_bindings_use_the_dispatched_spelling() {
        let profile = profile();
        let keystrokes = profile
            .fixed_bindings()
            .iter()
            .map(|binding| {
                binding
                    .keystrokes()
                    .iter()
                    .map(|keystroke| keystroke.inner().unparse())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>();
        for expected in [
            "ctrl-shift-c",
            "ctrl-insert",
            "ctrl-shift-v",
            "shift-insert",
            "ctrl-,",
            "ctrl-shift-q",
            "ctrl-shift-m",
            "f11",
        ] {
            assert!(
                keystrokes.iter().any(|keystroke| keystroke == expected),
                "{expected} missing from {keystrokes:?}"
            );
        }
    }

    #[test]
    fn linux_shift_insert_pastes_the_primary_selection_in_the_terminal() {
        let profile = profile();
        let shift_insert = [gpui::Keystroke::parse("shift-insert").unwrap()];
        let bindings = profile
            .fixed_bindings()
            .iter()
            .filter(|binding| binding.match_keystrokes(&shift_insert) == Some(false))
            .collect::<Vec<_>>();
        let [binding] = bindings[..] else {
            panic!("Shift+Insert should have one fixed binding");
        };
        assert!(binding.action().partial_eq(&crate::ui::PasteSelection));
        assert_eq!(
            binding.predicate().map(|predicate| predicate.to_string()),
            Some(TERMINAL_KEY_CONTEXT.to_owned())
        );
        assert_eq!(
            profile.check(&crate::keybindings::Shortcut::parse("shift-insert").unwrap()),
            Err(crate::keybindings::Reservation::System(
                crate::keybindings::SystemReservation::PasteSelection
            ))
        );
    }

    #[test]
    fn linux_application_palette_chord_can_be_assigned_to_a_command() {
        let profile = profile();
        let shortcut = crate::keybindings::Shortcut::parse("ctrl-shift-p").unwrap();
        assert!(profile.fixed_bindings().iter().all(|binding| {
            binding
                .match_keystrokes(&[gpui::Keystroke::parse("ctrl-shift-p").unwrap()])
                .is_none()
        }));
        let mut preferences = KeybindingPreferences::default();
        profile
            .assign(&mut preferences, Command::CreateTab, Some(shortcut.clone()))
            .unwrap();
        assert_eq!(
            profile.resolve(&preferences).shortcut(Command::CreateTab),
            Some(&shortcut)
        );
    }
}
