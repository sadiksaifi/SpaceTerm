//! Explicit application shortcut policy shared by host composition and test fixtures.
#![cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "the Command-key desktop composes this table; other desktops use it only as a test fixture")
)]
use crate::app::*;
use crate::keybindings::{
    Command, DefaultBinding, KeymapProfile, KeymapProfileError, SystemReserved,
};
use crate::ui::*;
use gpui::KeyBinding;
use spaceterm_ui::{EditCopy, EditPaste};

/// The Develop menu's application-wide Shortcuts. Each is System Reserved, so an assigned Command
/// can never be shadowed by one.
#[cfg(feature = "developer-tools")]
const OPEN_DEVELOPER_WORKBENCH: &str = "cmd-alt-a";
#[cfg(feature = "developer-tools")]
const TOGGLE_APPEARANCE_PREVIEW: &str = "cmd-alt-c";

pub(crate) fn profile(
    layout: std::rc::Rc<dyn crate::platform::keyboard_layout::KeyboardLayoutAdapter>,
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
        crate::keybindings::TerminalConventions::CommandShortcuts,
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
            KeyBinding::new(
                "cmd-w",
                crate::ui::developer_workbench::CloseDeveloperWorkbench,
                Some(crate::ui::developer_workbench::WORKBENCH_KEY_CONTEXT),
            ),
        ])
        .collect();
    bindings
}

#[cfg(feature = "developer-tools")]
fn developer_reservations() -> [SystemReserved; 2] {
    use crate::keybindings::{Shortcut, SystemReservation};

    [
        (OPEN_DEVELOPER_WORKBENCH, SystemReservation::DeveloperWorkbench),
        (TOGGLE_APPEARANCE_PREVIEW, SystemReservation::AppearancePreview),
    ]
    .map(|(source, reason)| SystemReserved {
        shortcut: Shortcut::parse(source).expect("static developer shortcut"),
        reason,
    })
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
