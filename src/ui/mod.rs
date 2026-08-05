mod overlay_scrollbar;
mod pane_action_menu;
mod pane_host;
mod terminal_element;
mod terminal_focus;
mod terminal_pane;
mod window_manager;
mod workspace_manager;

use gpui::{App, KeyBinding, MouseDownEvent, Window, actions};

pub(crate) use pane_action_menu::{
    CloseTarget, PANE_ACTION_MENU_HEIGHT, PANE_ACTION_MENU_WIDTH, PaneActionMenuCommand,
    render_pane_action_menu,
};
pub(crate) use pane_host::{PaneHost, PaneHostEvent};
pub(crate) use terminal_pane::{TerminalPane, TerminalPaneEvent};
pub(crate) use window_manager::{WindowManager, WindowManagerEvent};
pub(crate) use workspace_manager::WorkspaceManager;

actions!(
    terminal,
    [
        CopySelection,
        PasteClipboard,
        IncreaseTerminalFontSize,
        DecreaseTerminalFontSize,
        ResetTerminalFontSize,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        TogglePaneZoom,
        CreateWindow,
        ActivateWindow1,
        ActivateWindow2,
        ActivateWindow3,
        ActivateWindow4,
        ActivateWindow5,
        ActivateWindow6,
        ActivateWindow7,
        ActivateWindow8,
        ActivateWindow9,
        ActivateWorkspace1,
        ActivateWorkspace2,
        ActivateWorkspace3,
        ActivateWorkspace4,
        ActivateWorkspace5,
        ActivateWorkspace6,
        ActivateWorkspace7,
        ActivateWorkspace8,
        ActivateWorkspace9,
        ClosePane,
        CloseWindow,
        CreateWorkspace,
        ToggleSidebar,
        ToggleSidebarFocus
    ]
);

pub(crate) const TERMINAL_KEY_CONTEXT: &str = "TerminalPane";
pub(crate) const TOP_CHROME_HEIGHT: f32 = 36.0;
pub(crate) const WORKSPACE_SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;
pub(crate) const WORKSPACE_SIDEBAR_MINIMUM_WIDTH: f32 = 180.0;

pub(crate) fn handle_top_chrome_mouse_down(
    event: &MouseDownEvent,
    window: &mut Window,
    cx: &mut App,
) {
    match event.click_count {
        1 => window.start_window_move(),
        2 => window.titlebar_double_click(),
        _ => {}
    }
    cx.stop_propagation();
}

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-c", CopySelection, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", PasteClipboard, Some(TERMINAL_KEY_CONTEXT)),
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
        KeyBinding::new("cmd-t", CreateWindow, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-1", ActivateWindow1, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-2", ActivateWindow2, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-3", ActivateWindow3, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-4", ActivateWindow4, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-5", ActivateWindow5, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-6", ActivateWindow6, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-7", ActivateWindow7, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-8", ActivateWindow8, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-9", ActivateWindow9, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-1", ActivateWorkspace1, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-2", ActivateWorkspace2, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-3", ActivateWorkspace3, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-4", ActivateWorkspace4, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-5", ActivateWorkspace5, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-6", ActivateWorkspace6, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-7", ActivateWorkspace7, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-8", ActivateWorkspace8, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("ctrl-9", ActivateWorkspace9, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-w", ClosePane, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-w", CloseWindow, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-n", CreateWorkspace, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-b", ToggleSidebar, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-e",
            ToggleSidebarFocus,
            Some(TERMINAL_KEY_CONTEXT),
        ),
    ]);
}

#[cfg(test)]
mod tests {
    use gpui::{Action, Keystroke, TestAppContext};

    use super::*;

    #[gpui::test]
    fn terminal_zoom_shortcuts_should_bind_font_size_actions(cx: &mut TestAppContext) {
        cx.update(init);
        let expected = [
            ("cmd-=", IncreaseTerminalFontSize.name()),
            ("cmd-+", IncreaseTerminalFontSize.name()),
            ("cmd--", DecreaseTerminalFontSize.name()),
            ("cmd-0", ResetTerminalFontSize.name()),
        ];
        let actual = cx.update(|cx| {
            expected
                .iter()
                .map(|(shortcut, _)| {
                    let keystroke = Keystroke::parse(shortcut).unwrap_or_else(|error| {
                        panic!("invalid test shortcut {shortcut}: {error}")
                    });
                    let bindings = cx.all_bindings_for_input(&[keystroke]);
                    (
                        *shortcut,
                        bindings
                            .last()
                            .map(|binding| binding.action().name())
                            .unwrap_or(""),
                    )
                })
                .collect::<Vec<_>>()
        });

        assert_eq!(actual.as_slice(), expected);
    }
}
