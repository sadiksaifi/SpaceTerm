//! Explicit application shortcut policy shared by host composition and test fixtures.
use crate::app::*;
use crate::keybindings::{
    Command, DefaultBinding, KeymapProfile, KeymapProfileError, SystemReserved,
};
use crate::ui::*;
use gpui::KeyBinding;
use spaceterm_ui::{EditCopy, EditPaste};

pub(crate) fn profile(
    layout: std::rc::Rc<dyn crate::platform::keyboard_layout::KeyboardLayoutAdapter>,
    system_reserved: Vec<SystemReserved>,
) -> Result<KeymapProfile, KeymapProfileError> {
    let defaults = [
        (
            Command::SwitchWorkspace,
            Some(DefaultBinding::new("cmd-shift-k", &[])),
        ),
        (
            Command::NewWorkspace,
            Some(DefaultBinding::new("cmd-n", &[])),
        ),
        (
            Command::NewRemoteWorkspace,
            Some(DefaultBinding::new("cmd-shift-n", &[])),
        ),
        (
            Command::OpenLocalDirectory,
            Some(DefaultBinding::new("cmd-o", &[])),
        ),
        (
            Command::OpenRemoteDirectory,
            Some(DefaultBinding::new("cmd-shift-o", &[])),
        ),
        (Command::CloseWorkspace, None),
        (
            Command::ActivateWorkspace1,
            Some(DefaultBinding::new("ctrl-1", &[])),
        ),
        (
            Command::ActivateWorkspace2,
            Some(DefaultBinding::new("ctrl-2", &[])),
        ),
        (
            Command::ActivateWorkspace3,
            Some(DefaultBinding::new("ctrl-3", &[])),
        ),
        (
            Command::ActivateWorkspace4,
            Some(DefaultBinding::new("ctrl-4", &[])),
        ),
        (
            Command::ActivateWorkspace5,
            Some(DefaultBinding::new("ctrl-5", &[])),
        ),
        (
            Command::ActivateWorkspace6,
            Some(DefaultBinding::new("ctrl-6", &[])),
        ),
        (
            Command::ActivateWorkspace7,
            Some(DefaultBinding::new("ctrl-7", &[])),
        ),
        (
            Command::ActivateWorkspace8,
            Some(DefaultBinding::new("ctrl-8", &[])),
        ),
        (
            Command::ActivateWorkspace9,
            Some(DefaultBinding::new("ctrl-9", &[])),
        ),
        (Command::CreateTab, Some(DefaultBinding::new("cmd-t", &[]))),
        (
            Command::CloseTab,
            Some(DefaultBinding::new("cmd-shift-w", &[])),
        ),
        (
            Command::ActivateTab1,
            Some(DefaultBinding::new("cmd-1", &[])),
        ),
        (
            Command::ActivateTab2,
            Some(DefaultBinding::new("cmd-2", &[])),
        ),
        (
            Command::ActivateTab3,
            Some(DefaultBinding::new("cmd-3", &[])),
        ),
        (
            Command::ActivateTab4,
            Some(DefaultBinding::new("cmd-4", &[])),
        ),
        (
            Command::ActivateTab5,
            Some(DefaultBinding::new("cmd-5", &[])),
        ),
        (
            Command::ActivateTab6,
            Some(DefaultBinding::new("cmd-6", &[])),
        ),
        (
            Command::ActivateTab7,
            Some(DefaultBinding::new("cmd-7", &[])),
        ),
        (
            Command::ActivateTab8,
            Some(DefaultBinding::new("cmd-8", &[])),
        ),
        (
            Command::ActivateTab9,
            Some(DefaultBinding::new("cmd-9", &[])),
        ),
        (Command::ClosePane, Some(DefaultBinding::new("cmd-w", &[]))),
        (Command::SplitRight, Some(DefaultBinding::new("cmd-d", &[]))),
        (
            Command::SplitDown,
            Some(DefaultBinding::new("cmd-shift-d", &[])),
        ),
        (
            Command::FocusPaneLeft,
            Some(DefaultBinding::new("cmd-alt-left", &[])),
        ),
        (
            Command::FocusPaneRight,
            Some(DefaultBinding::new("cmd-alt-right", &[])),
        ),
        (
            Command::FocusPaneUp,
            Some(DefaultBinding::new("cmd-alt-up", &[])),
        ),
        (
            Command::FocusPaneDown,
            Some(DefaultBinding::new("cmd-alt-down", &[])),
        ),
        (
            Command::FocusPreviousPane,
            Some(DefaultBinding::new("cmd-[", &[])),
        ),
        (
            Command::FocusNextPane,
            Some(DefaultBinding::new("cmd-]", &[])),
        ),
        (
            Command::TogglePaneZoom,
            Some(DefaultBinding::new("cmd-shift-enter", &[])),
        ),
        (
            Command::OpenTerminalFind,
            Some(DefaultBinding::new("cmd-f", &[])),
        ),
        (Command::FindNext, Some(DefaultBinding::new("cmd-g", &[]))),
        (
            Command::FindPrevious,
            Some(DefaultBinding::new("cmd-shift-g", &[])),
        ),
        (
            Command::ClearTerminalScreenAndScrollback,
            Some(DefaultBinding::new("cmd-k", &[])),
        ),
        (
            Command::IncreaseTerminalFontSize,
            Some(DefaultBinding::new("cmd-=", &["cmd-+", "shift-cmd-="])),
        ),
        (
            Command::DecreaseTerminalFontSize,
            Some(DefaultBinding::new("cmd--", &[])),
        ),
        (
            Command::ResetTerminalFontSize,
            Some(DefaultBinding::new("cmd-0", &[])),
        ),
        (
            Command::ToggleSidebar,
            Some(DefaultBinding::new("cmd-b", &[])),
        ),
        (
            Command::ToggleSidebarFocus,
            Some(DefaultBinding::new("cmd-shift-e", &[])),
        ),
    ];
    KeymapProfile::new(
        layout,
        defaults,
        system_reserved,
        fixed_bindings(),
        control_bindings(),
    )
}

fn fixed_bindings() -> Vec<KeyBinding> {
    let bindings = vec![
        KeyBinding::new("cmd-c", EditCopy, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-,", crate::ui::settings_window::OpenSettings, None),
        KeyBinding::new("cmd-q", QuitApplication, None),
        KeyBinding::new("cmd-h", HideApplication, None),
        KeyBinding::new("alt-cmd-h", HideOtherApplications, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("fn-f", ToggleFullScreen, None),
    ];
    #[cfg(feature = "appearance-exerciser")]
    let bindings = bindings
        .into_iter()
        .chain([
            KeyBinding::new(
                "cmd-alt-a",
                crate::ui::appearance_exerciser::ShowAppearanceExerciser,
                None,
            ),
            KeyBinding::new(
                "cmd-alt-c",
                crate::ui::appearance_exerciser::ToggleAppearancePreview,
                None,
            ),
        ])
        .collect();
    bindings
}

fn control_bindings() -> Vec<KeyBinding> {
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
        KeyBinding::new(
            "cmd-w",
            crate::ui::settings_window::CloseSettingsWindow,
            Some(crate::ui::settings_window::SETTINGS_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-f",
            crate::ui::settings_window::FocusSettingsSearch,
            Some(crate::ui::settings_window::SETTINGS_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            crate::ui::settings_window::ClearSettingsSearch,
            Some(crate::ui::settings_window::SETTINGS_KEY_CONTEXT),
        ),
    ]
}
