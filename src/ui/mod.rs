mod pane_host;
mod terminal_element;
mod terminal_pane;

use gpui::{App, KeyBinding, actions};

pub(crate) use pane_host::PaneHost;
pub(crate) use terminal_pane::{TerminalPane, TerminalPaneEvent};

actions!(
    terminal,
    [
        CopySelection,
        PasteClipboard,
        SplitRight,
        SplitDown,
        TogglePaneZoom,
        ClosePane
    ]
);

pub(crate) const TERMINAL_KEY_CONTEXT: &str = "TerminalPane";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", PasteClipboard, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-d", SplitRight, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-d", SplitDown, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-enter",
            TogglePaneZoom,
            Some(TERMINAL_KEY_CONTEXT),
        ),
        KeyBinding::new("cmd-w", ClosePane, Some(TERMINAL_KEY_CONTEXT)),
    ]);
}
