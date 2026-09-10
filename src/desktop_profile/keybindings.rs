//! Explicit application shortcut policy shared by host composition and test fixtures.
use crate::app::*;
use crate::ui::*;
use gpui::KeyBinding;
use spaceterm_ui::{EditCopy, EditPaste};
pub(crate) fn bindings() -> Vec<KeyBinding> {
    let mut bindings = vec![
        KeyBinding::new("cmd-k", SwitchWorkspace, None),
        KeyBinding::new("cmd-n", NewWorkspace, None),
        KeyBinding::new("cmd-shift-n", NewRemoteWorkspace, None),
        KeyBinding::new("cmd-t", CreateTab, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("cmd-shift-w", CloseTab, None),
        KeyBinding::new("cmd-c", EditCopy, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-f", OpenTerminalFind, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-g", FindNext, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-f", OpenTerminalFind, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("cmd-g", FindNext, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(TERMINAL_FIND_KEY_CONTEXT)),
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
            "cmd-=",
            IncreaseTerminalFontSize,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "cmd-+",
            IncreaseTerminalFontSize,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "cmd--",
            DecreaseTerminalFontSize,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new("cmd-0", ResetTerminalFontSize, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-d", SplitRight, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-d", SplitDown, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-h", FocusPaneLeft, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-left", FocusPaneLeft, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-l", FocusPaneRight, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-right", FocusPaneRight, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-k", FocusPaneUp, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-up", FocusPaneUp, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-j", FocusPaneDown, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-alt-down", FocusPaneDown, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-enter",
            TogglePaneZoom,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new("cmd-1", ActivateTab1, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-2", ActivateTab2, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-3", ActivateTab3, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-4", ActivateTab4, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-5", ActivateTab5, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-6", ActivateTab6, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-7", ActivateTab7, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-8", ActivateTab8, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-9", ActivateTab9, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-1", ActivateWorkspace1, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-2", ActivateWorkspace2, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-3", ActivateWorkspace3, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-4", ActivateWorkspace4, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-5", ActivateWorkspace5, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-6", ActivateWorkspace6, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-7", ActivateWorkspace7, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-8", ActivateWorkspace8, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-9", ActivateWorkspace9, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-e",
            ToggleSidebarFocus,
            Some(TERMINAL_KEY_CONTEXT),
        ),
    ];
    bindings.extend([
        KeyBinding::new("cmd-q", QuitApplication, None),
        KeyBinding::new("cmd-h", HideApplication, None),
        KeyBinding::new("alt-cmd-h", HideOtherApplications, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("fn-f", ToggleFullScreen, None),
    ]);
    bindings
}
