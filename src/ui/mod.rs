mod terminal_element;
mod terminal_pane;

use gpui::{App, KeyBinding, actions};

pub(crate) use terminal_pane::TerminalPane;

actions!(terminal, [CopySelection, PasteClipboard]);

pub(crate) const TERMINAL_KEY_CONTEXT: &str = "TerminalPane";

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", PasteClipboard, Some(TERMINAL_KEY_CONTEXT)),
    ]);
}
