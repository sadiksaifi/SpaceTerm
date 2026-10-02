//! The Linux application Shortcut policy: Commands take Ctrl+Shift, as in VTE terminals, so
//! plain Control and Alt reach programs in the terminal and Super stays with the desktop.
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

pub(super) fn profile(
    layout: Rc<dyn KeyboardLayoutAdapter>,
    system_reserved: Vec<SystemReserved>,
) -> Result<KeymapProfile, KeymapProfileError> {
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
            Some(DefaultBinding::new("ctrl-shift-alt-1", &[])),
        ),
        (
            Command::ActivateWorkspace2,
            Some(DefaultBinding::new("ctrl-shift-alt-2", &[])),
        ),
        (
            Command::ActivateWorkspace3,
            Some(DefaultBinding::new("ctrl-shift-alt-3", &[])),
        ),
        (
            Command::ActivateWorkspace4,
            Some(DefaultBinding::new("ctrl-shift-alt-4", &[])),
        ),
        (
            Command::ActivateWorkspace5,
            Some(DefaultBinding::new("ctrl-shift-alt-5", &[])),
        ),
        (
            Command::ActivateWorkspace6,
            Some(DefaultBinding::new("ctrl-shift-alt-6", &[])),
        ),
        (
            Command::ActivateWorkspace7,
            Some(DefaultBinding::new("ctrl-shift-alt-7", &[])),
        ),
        (
            Command::ActivateWorkspace8,
            Some(DefaultBinding::new("ctrl-shift-alt-8", &[])),
        ),
        (
            Command::ActivateWorkspace9,
            Some(DefaultBinding::new("ctrl-shift-alt-9", &[])),
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
            Some(DefaultBinding::new("ctrl-shift-1", &[])),
        ),
        (
            Command::ActivateTab2,
            Some(DefaultBinding::new("ctrl-shift-2", &[])),
        ),
        (
            Command::ActivateTab3,
            Some(DefaultBinding::new("ctrl-shift-3", &[])),
        ),
        (
            Command::ActivateTab4,
            Some(DefaultBinding::new("ctrl-shift-4", &[])),
        ),
        (
            Command::ActivateTab5,
            Some(DefaultBinding::new("ctrl-shift-5", &[])),
        ),
        (
            Command::ActivateTab6,
            Some(DefaultBinding::new("ctrl-shift-6", &[])),
        ),
        (
            Command::ActivateTab7,
            Some(DefaultBinding::new("ctrl-shift-7", &[])),
        ),
        (
            Command::ActivateTab8,
            Some(DefaultBinding::new("ctrl-shift-8", &[])),
        ),
        (
            Command::ActivateTab9,
            Some(DefaultBinding::new("ctrl-shift-9", &[])),
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
            Some(DefaultBinding::new("ctrl-shift-alt-f6", &["ctrl-shift-["])),
        ),
        (
            Command::FocusNextPane,
            Some(DefaultBinding::new("ctrl-shift-f6", &["ctrl-shift-]"])),
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
            Some(DefaultBinding::new("ctrl-shift-g", &[])),
        ),
        (
            Command::FindPrevious,
            Some(DefaultBinding::new("ctrl-shift-alt-g", &[])),
        ),
        (
            Command::ClearTerminalScreenAndScrollback,
            Some(DefaultBinding::new("ctrl-shift-alt-k", &[])),
        ),
        (
            Command::IncreaseTerminalFontSize,
            Some(DefaultBinding::new(
                "ctrl-shift-pageup",
                &["ctrl-shift-+", "ctrl-shift-="],
            )),
        ),
        (
            Command::DecreaseTerminalFontSize,
            Some(DefaultBinding::new(
                "ctrl-shift-pagedown",
                &["ctrl-shift--"],
            )),
        ),
        (
            Command::ResetTerminalFontSize,
            Some(DefaultBinding::new("ctrl-shift-0", &[])),
        ),
        (
            Command::ToggleSidebar,
            Some(DefaultBinding::new("ctrl-shift-b", &[])),
        ),
        (
            Command::ToggleSidebarFocus,
            Some(DefaultBinding::new("ctrl-shift-e", &[])),
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
        KeyBinding::new("ctrl-shift-v", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("shift-insert", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "ctrl-shift-,",
            crate::ui::settings_window::OpenSettings,
            None,
        ),
        KeyBinding::new("ctrl-shift-q", QuitApplication, None),
        KeyBinding::new("f11", ToggleFullScreen, None),
    ];
    #[cfg(feature = "appearance-exerciser")]
    let bindings = bindings
        .into_iter()
        .chain([
            KeyBinding::new(
                "ctrl-shift-alt-a",
                crate::ui::appearance_exerciser::ShowAppearanceExerciser,
                None,
            ),
            KeyBinding::new(
                "ctrl-shift-alt-c",
                crate::ui::appearance_exerciser::ToggleAppearancePreview,
                None,
            ),
        ])
        .collect();
    bindings
}

fn control_bindings() -> Vec<KeyBinding> {
    let settings = Some(crate::ui::settings_window::SETTINGS_KEY_CONTEXT);
    vec![
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
    ]
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
    fn linux_fixed_shortcuts_follow_the_active_layout() {
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
        assert_eq!(settings.keystrokes()[0].inner().key, ";");
        assert_eq!(
            settings.keystrokes()[0].inner().modifiers,
            gpui::Modifiers::control()
        );
    }

    #[test]
    fn linux_default_commands_resolve_on_us_german_and_french_layouts() {
        use crate::keybindings::KeybindingState;
        use crate::platform::keyboard_layout::testing;
        for (layout, reset) in [
            (KeyboardLayout::us_english(), "ctrl-)"),
            (testing::de(), "ctrl-="),
            (testing::fr_azerty(), "ctrl-0"),
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
            assert_eq!(
                resolved
                    .shortcut(Command::IncreaseTerminalFontSize)
                    .unwrap()
                    .to_string(),
                "ctrl-shift-pageup"
            );
            assert_eq!(
                resolved
                    .shortcut(Command::DecreaseTerminalFontSize)
                    .unwrap()
                    .to_string(),
                "ctrl-shift-pagedown"
            );
            assert_eq!(
                resolved
                    .shortcut(Command::ResetTerminalFontSize)
                    .unwrap()
                    .to_string(),
                reset
            );
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
            (Command::CreateTab, "ctrl-shift-t"),
            (Command::NewRemoteWorkspace, "ctrl-alt-shift-n"),
            (Command::ActivateTab1, "ctrl-!"),
            (Command::ActivateTab9, "ctrl-("),
            (Command::ActivateWorkspace1, "ctrl-alt-!"),
            (Command::FocusPreviousPane, "ctrl-alt-shift-f6"),
            (Command::FocusNextPane, "ctrl-shift-f6"),
            (Command::IncreaseTerminalFontSize, "ctrl-shift-pageup"),
            (Command::DecreaseTerminalFontSize, "ctrl-shift-pagedown"),
            (Command::ResetTerminalFontSize, "ctrl-)"),
            (Command::FocusPaneLeft, "ctrl-shift-left"),
        ] {
            assert_eq!(
                resolved
                    .shortcut(command)
                    .map(ToString::to_string)
                    .as_deref(),
                Some(dispatch),
                "{command:?}"
            );
        }
        assert_eq!(resolved.shortcut(Command::CloseWorkspace), None);
        assert_eq!(
            resolved.shortcuts(Command::IncreaseTerminalFontSize).len(),
            2,
            "aliases that resolve to the same chord are deduplicated"
        );
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
            "ctrl-shift-v",
            "shift-insert",
            "ctrl-<",
            "ctrl-shift-q",
            "f11",
        ] {
            assert!(
                keystrokes.iter().any(|keystroke| keystroke == expected),
                "{expected} missing from {keystrokes:?}"
            );
        }
    }
}
