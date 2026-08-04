mod pane_action_menu;
mod pane_host;
mod terminal_element;
mod terminal_pane;
mod window_manager;

use gpui::{App, KeyBinding, actions};

pub(crate) use pane_action_menu::{
    CloseTarget, PANE_ACTION_MENU_HEIGHT, PANE_ACTION_MENU_WIDTH, PaneActionMenuCommand,
    render_pane_action_menu,
};
pub(crate) use pane_host::{PaneHost, PaneHostEvent};
pub(crate) use terminal_pane::{TerminalPane, TerminalPaneEvent};
pub(crate) use window_manager::WindowManager;

actions!(
    terminal,
    [
        CopySelection,
        PasteClipboard,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        TogglePaneZoom,
        ClosePane,
        CloseWindow
    ]
);

pub(crate) const TERMINAL_KEY_CONTEXT: &str = "TerminalPane";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", PasteClipboard, Some(TERMINAL_KEY_CONTEXT)),
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
        KeyBinding::new("cmd-w", ClosePane, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-w", CloseWindow, Some(TERMINAL_KEY_CONTEXT)),
    ]);
}
