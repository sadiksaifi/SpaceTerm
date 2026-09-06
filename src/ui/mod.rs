mod button_theme;
mod close_policy;
mod command_palette_theme;
mod control_theme_catalog;
mod menu_theme;
mod modal_theme;
mod native_remote_workspace_flow_backend;
mod new_workspace_panel;
mod pane_action_menu;
mod pane_host;
pub(crate) mod pane_lifecycle;
mod remote_child_launch;
pub(crate) mod remote_workspace_flow;
pub(crate) mod remote_workspace_picker;
mod render_lifecycle;
mod resize_handle_theme;
mod scrollbar_theme;
pub(crate) mod ssh_askpass_dialog;
mod ssh_host_form;
mod ssh_host_picker;
mod tab_manager;
mod terminal_context_menu;
mod terminal_element;
mod terminal_focus;
mod terminal_graphics;
mod terminal_ime;
mod terminal_pane;
mod terminal_symbols;
mod text_input_theme;
mod tooltip_theme;
mod workspace_manager;
mod workspace_picker;
mod workspace_search;

use gpui::{App, actions};

pub(crate) use native_remote_workspace_flow_backend::NativeRemoteWorkspaceFlowBackendFactory;
pub(crate) use pane_host::{
    PaneHost, PaneHostEvent, PreparedPaneHostRemoteRestart, RemotePaneHostLifecycleError,
};
pub(crate) use remote_child_launch::RemoteChildLaunchUnavailable;
#[cfg(test)]
pub(crate) use render_lifecycle::{RenderLifecycle, ScaleChange, SurfaceVisibility};
pub(crate) use tab_manager::{TabManager, TabManagerEvent};
#[cfg(test)]
pub(crate) use terminal_focus::{
    TerminalFocusBlocker, TerminalFocusCoordinator, TerminalFocusFacts,
};
#[cfg(test)]
pub(crate) use terminal_ime::conformance_ime_observation;
pub(crate) use terminal_pane::{
    PreparedRemotePaneRestart, RemotePaneLifecycleError, TerminalPane, TerminalPaneEvent,
};
pub(crate) use workspace_manager::{WorkspaceManager, WorkspaceManagerAdapters};

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
        CreateTab,
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateTab9,
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
        CloseTab,
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

fn workspace_count_summary(tab_count: usize, pane_count: usize) -> String {
    let tab_label = if tab_count == 1 { "tab" } else { "tabs" };
    let pane_label = if pane_count == 1 { "pane" } else { "panes" };
    format!("{tab_count} {tab_label} · {pane_count} {pane_label}")
}

pub(crate) fn initialize_controls(cx: &mut App) -> gpui::Result<()> {
    spaceterm_ui::init(cx, control_theme_catalog::catalog())
}

#[cfg(test)]
pub(crate) fn init(cx: &mut App) -> gpui::Result<()> {
    init_with_text_direction(cx, spaceterm_ui::TextDirection::LeftToRight)
}

#[cfg(test)]
fn init_with_text_direction(
    cx: &mut App,
    direction: spaceterm_ui::TextDirection,
) -> gpui::Result<()> {
    initialize_controls(cx)?;
    crate::desktop_profile::testing_profile(direction).install(cx);
    Ok(())
}

#[cfg(test)]
mod tests {
    use gpui::{Action, Keystroke, TestAppContext};

    use super::*;

    #[test]
    fn workspace_count_summary_should_pluralize_each_entity_independently() {
        assert_eq!(
            (
                workspace_count_summary(1, 1),
                workspace_count_summary(1, 2),
                workspace_count_summary(2, 1),
                workspace_count_summary(2, 3),
            ),
            (
                "1 tab · 1 pane".to_owned(),
                "1 tab · 2 panes".to_owned(),
                "2 tabs · 1 pane".to_owned(),
                "2 tabs · 3 panes".to_owned(),
            )
        );
    }

    #[gpui::test]
    fn ui_init_should_install_control_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init_with_text_direction(cx, spaceterm_ui::TextDirection::LeftToRight)
                .expect("UI initialization should succeed")
        });

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
        cx.update(|cx| init(cx).expect("UI initialization should succeed"));
        let command_period =
            Keystroke::parse("cmd-.").expect("macOS modal key equivalent should parse");

        let has_binding = cx.update(|cx| !cx.all_bindings_for_input(&[command_period]).is_empty());

        assert!(has_binding);
    }

    #[gpui::test]
    fn terminal_zoom_shortcuts_should_bind_font_size_actions(cx: &mut TestAppContext) {
        cx.update(|cx| init(cx).expect("UI initialization should succeed"));
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
        cx.update(|cx| init(cx).expect("UI initialization should succeed"));
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
        cx.update(|cx| init(cx).expect("UI initialization should succeed"));
        let expected = [
            ("cmd-shift-n", CreateScratchWorkspace.name()),
            ("cmd-p", SearchWorkspaces.name()),
            ("cmd-n", ShowNewWorkspacePanel.name()),
            ("cmd-o", OpenLocalProject.name()),
            ("cmd-t", CreateTab.name()),
            ("cmd-w", ClosePane.name()),
            ("cmd-shift-w", CloseTab.name()),
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
