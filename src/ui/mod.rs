mod button_theme;
mod command_palette_theme;
mod control_theme_catalog;
mod menu_theme;
mod modal_theme;
mod native_remote_workspace_flow_backend;
mod new_workspace_panel;
mod pane_action_menu;
mod pane_host;
mod remote_child_launch;
mod remote_workspace_flow;
pub(crate) mod remote_workspace_picker;
mod render_lifecycle;
mod resize_handle_theme;
mod scrollbar_theme;
mod ssh_host_form;
mod ssh_host_picker;
mod terminal_context_menu;
mod terminal_element;
mod terminal_focus;
mod terminal_graphics;
mod terminal_ime;
mod terminal_pane;
mod terminal_symbols;
mod text_input_theme;
mod tooltip_theme;
mod window_manager;
mod workspace_manager;
mod workspace_picker;
mod workspace_search;

use gpui::{App, KeyBinding, actions};
use spaceterm_ui::{EditCopy, EditPaste};

pub(crate) use native_remote_workspace_flow_backend::NativeRemoteWorkspaceFlowBackendFactory;
pub(crate) use pane_host::{
    PaneHost, PaneHostEvent, PreparedPaneHostRemoteRestart, RemotePaneHostLifecycleError,
};
pub(crate) use remote_child_launch::RemoteChildLaunchUnavailable;
#[cfg(test)]
pub(crate) use render_lifecycle::{RenderLifecycle, ScaleChange, SurfaceVisibility};
#[cfg(test)]
pub(crate) use terminal_focus::{
    TerminalFocusBlocker, TerminalFocusCoordinator, TerminalFocusFacts,
};
#[cfg(test)]
pub(crate) use terminal_ime::conformance_ime_observation;
pub(crate) use terminal_pane::{
    PreparedRemotePaneRestart, RemotePaneLifecycleError, TerminalPane, TerminalPaneEvent,
};
pub(crate) use window_manager::{WindowManager, WindowManagerEvent};
pub(crate) use workspace_manager::WorkspaceManager;

actions!(
    terminal,
    [
        CopySelection,
        PasteClipboard,
        ConfirmUnsafePaste,
        CancelUnsafePaste,
        AllowOsc52Clipboard,
        DenyOsc52Clipboard,
        ExportTerminalDiagnostics,
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
        CloseWorkspace,
        CreateScratchWorkspace,
        SearchWorkspaces,
        ShowNewWorkspacePanel,
        OpenLocalProject,
        ToggleSidebar,
        ToggleSidebarFocus,
        OpenTerminalFind,
        FindNext,
        FindPrevious,
        CloseTerminalFind,
        FocusNextTerminalFindControl,
        FocusPreviousTerminalFindControl
    ]
);

pub(crate) const TERMINAL_KEY_CONTEXT: &str = "TerminalPane";
pub(crate) const TERMINAL_FIND_KEY_CONTEXT: &str = "TerminalFind";
pub(crate) const TERMINAL_PASTE_CONFIRMATION_KEY_CONTEXT: &str = "TerminalPasteConfirmation";
pub(crate) const TERMINAL_OSC52_AUTHORIZATION_KEY_CONTEXT: &str = "TerminalOsc52Authorization";
pub(crate) const TOP_CHROME_HEIGHT: f32 = 36.0;
pub(crate) const WORKSPACE_SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;
pub(crate) const WORKSPACE_SIDEBAR_MINIMUM_WIDTH: f32 = 180.0;

pub(crate) fn init(cx: &mut App) {
    init_with_text_direction(cx, crate::platform::macos_locale::current_text_direction());
}

fn init_with_text_direction(cx: &mut App, text_direction: spaceterm_ui::TextDirection) {
    spaceterm_ui::init(cx, control_theme_catalog::catalog());
    spaceterm_ui::install_modal_policy(
        cx,
        spaceterm_ui::ModalDesktopPolicy::mac_os().with_text_direction(text_direction),
    );
    spaceterm_ui::install_modal_keybindings(cx, spaceterm_ui::ModalKeybindingProfile::MacOs);
    spaceterm_ui::install_text_input_keybindings(
        cx,
        spaceterm_ui::TextInputKeybindingProfile::MacOs,
    );
    cx.bind_keys([
        KeyBinding::new("cmd-n", CreateScratchWorkspace, None),
        KeyBinding::new("cmd-p", SearchWorkspaces, None),
        KeyBinding::new("cmd-o", ShowNewWorkspacePanel, None),
        KeyBinding::new("shift-cmd-o", OpenLocalProject, None),
        KeyBinding::new("cmd-t", CreateWindow, None),
        KeyBinding::new("cmd-w", ClosePane, None),
        KeyBinding::new("cmd-shift-w", CloseWindow, None),
        KeyBinding::new("cmd-c", EditCopy, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-v", EditPaste, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-f", OpenTerminalFind, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-g", FindNext, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(TERMINAL_KEY_CONTEXT)),
        KeyBinding::new("cmd-f", OpenTerminalFind, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("cmd-g", FindNext, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("cmd-shift-g", FindPrevious, Some(TERMINAL_FIND_KEY_CONTEXT)),
        KeyBinding::new("enter", FindNext, Some(TERMINAL_FIND_KEY_CONTEXT)),
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
            "cmd-enter",
            AllowOsc52Clipboard,
            Some(TERMINAL_OSC52_AUTHORIZATION_KEY_CONTEXT),
        ),
        KeyBinding::new(
            "escape",
            DenyOsc52Clipboard,
            Some(TERMINAL_OSC52_AUTHORIZATION_KEY_CONTEXT),
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
    fn ui_init_should_install_control_themes(cx: &mut TestAppContext) {
        cx.update(|cx| init_with_text_direction(cx, spaceterm_ui::TextDirection::LeftToRight));

        assert!(cx.update(|cx| {
            cx.has_global::<spaceterm_ui::ButtonTheme>()
                && cx.has_global::<spaceterm_ui::ScrollbarTheme>()
                && cx.has_global::<spaceterm_ui::ResizeHandleTheme>()
                && cx.has_global::<spaceterm_ui::MenuTheme>()
                && cx.has_global::<spaceterm_ui::CommandPaletteTheme>()
                && cx.has_global::<spaceterm_ui::TextInputTheme>()
                && cx.has_global::<spaceterm_ui::TooltipTheme>()
                && cx.has_global::<spaceterm_ui::ModalTheme>()
                && *cx.global::<spaceterm_ui::ModalTheme>() == modal_theme::theme()
                && cx.has_global::<spaceterm_ui::ModalDesktopPolicy>()
                && *cx.global::<spaceterm_ui::ModalDesktopPolicy>()
                    == spaceterm_ui::ModalDesktopPolicy::mac_os()
        }));
    }

    #[gpui::test]
    fn ui_init_should_install_macos_modal_command_period_binding(cx: &mut TestAppContext) {
        cx.update(init);
        let command_period =
            Keystroke::parse("cmd-.").expect("macOS modal key equivalent should parse");

        let has_binding = cx.update(|cx| !cx.all_bindings_for_input(&[command_period]).is_empty());

        assert!(has_binding);
    }

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

    #[gpui::test]
    fn terminal_find_shortcuts_should_bind_standard_macos_actions(cx: &mut TestAppContext) {
        cx.update(init);
        let expected = [
            ("cmd-f", OpenTerminalFind.name()),
            ("cmd-g", FindNext.name()),
            ("cmd-shift-g", FindPrevious.name()),
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

    #[gpui::test]
    fn workspace_and_hierarchy_shortcuts_should_be_global(cx: &mut TestAppContext) {
        cx.update(init);
        let expected = [
            ("cmd-n", CreateScratchWorkspace.name()),
            ("cmd-p", SearchWorkspaces.name()),
            ("cmd-o", ShowNewWorkspacePanel.name()),
            ("shift-cmd-o", OpenLocalProject.name()),
            ("cmd-t", CreateWindow.name()),
            ("cmd-w", ClosePane.name()),
            ("cmd-shift-w", CloseWindow.name()),
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
