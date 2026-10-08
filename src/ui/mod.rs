pub(crate) mod about_window;
pub(crate) mod appearance;
pub(crate) mod appearance_runtime;
pub(crate) mod application_prompt;
pub(crate) mod askpass_dialog;
pub(crate) mod chrome_geometry;
pub(crate) mod chrome_icons;
mod chrome_semantic_pairs;
mod chrome_state;
pub(crate) mod chrome_typography;
mod control_theme;
#[cfg(feature = "developer-tools")]
pub(crate) mod developer_workbench;
pub(crate) mod directory_picker;
mod drag_and_drop;
mod native_remote_workspace_flow_backend;
pub(crate) mod pane_lifecycle;
pub(crate) mod permission_setup;
mod remote_child_launch;
pub(crate) mod remote_workspace_flow;
mod render_lifecycle;
pub(crate) mod repository_status_store;
mod selection_chip;
pub(crate) mod settings_file;
pub(crate) mod settings_recovery;
pub(crate) mod settings_window;
mod sidebar_window;
mod ssh_host_form;
mod ssh_host_picker;
mod tab_manager;
mod tab_view;
mod terminal_context_menu;
mod terminal_element;
mod terminal_focus;
mod terminal_graphics;
mod terminal_ime;
mod terminal_pane;
mod terminal_status;
mod terminal_symbols;
pub(crate) mod updates;
mod window_shell;
mod workspace_chrome;
mod workspace_creation;
mod workspace_frame;
mod workspace_manager;
mod workspace_sidebar;
mod workspace_status;
pub(crate) mod worktree_store;

use gpui::{App, actions};

pub(crate) use native_remote_workspace_flow_backend::{
    NativeRemoteWorkspaceFlowBackendFactory, RemoteWorkspaceSshRuntime,
};
pub(crate) use remote_child_launch::RemoteChildLaunchUnavailable;
#[cfg(test)]
pub(crate) use render_lifecycle::{RenderLifecycle, ScaleChange, SurfaceVisibility};
pub(crate) use tab_manager::{TabManager, TabManagerEvent};
pub(crate) use tab_view::{
    PreparedTabViewRemoteRestart, RemoteTabViewLifecycleError, TabIdentity, TabView, TabViewEvent,
};
#[cfg(test)]
pub(crate) use terminal_focus::{
    TerminalFocusBlocker, TerminalFocusCoordinator, TerminalFocusFacts,
};
pub(crate) use terminal_pane::{
    PaneOrigin, PreparedRemotePaneRestart, RemotePaneLifecycleError, TerminalPane,
    TerminalPaneEvent,
};
pub(crate) use workspace_frame::WorkspaceFrame;
#[cfg(test)]
pub(crate) use workspace_manager::tests::assert_scroll_shortcuts_from_sidebar_focus;
pub(crate) use workspace_manager::{WorkspaceManager, WorkspaceManagerAdapters};

/// Finishes every hover transition in progress, since test windows have no frame loop.
#[cfg(test)]
pub(crate) fn settle_hover(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.executor()
        .advance_clock(std::time::Duration::from_millis(220));
    cx.update(|window, cx| window.simulate_next_frame(cx));
    cx.run_until_parked();
}

actions!(
    terminal,
    [
        CopySelection,
        PasteClipboard,
        PasteSelection,
        ConfirmUnsafePaste,
        CancelUnsafePaste,
        SetUpPermissionRequest,
        DeclinePermissionRequest,
        ExportTerminalDiagnostics,
        IncreaseTerminalFontSize,
        DecreaseTerminalFontSize,
        ResetTerminalFontSize,
        ClearTerminalScreenAndScrollback,
        ScrollPageUp,
        ScrollPageDown,
        ScrollToTop,
        ScrollToBottom,
        SplitRight,
        SplitDown,
        FocusPaneLeft,
        FocusPaneRight,
        FocusPaneUp,
        FocusPaneDown,
        FocusPreviousPane,
        FocusNextPane,
        TogglePaneZoom,
        ShowRepositoryStatus,
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
        NextTab,
        PreviousTab,
        MoveTabRight,
        MoveTabLeft,
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
        SwitchWorkspace,
        NewWorkspace,
        NewRemoteWorkspace,
        OpenLocalDirectory,
        OpenRemoteDirectory,
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
/// Added to a Pane's key context while it offers a Permission Request.
pub(crate) const TERMINAL_PERMISSION_REQUEST_KEY_CONTEXT: &str = "TerminalPermissionRequest";
#[cfg(test)]
pub(crate) const TOP_CHROME_HEIGHT: f32 = 36.0;
pub(crate) const WORKSPACE_SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;
pub(crate) const WORKSPACE_SIDEBAR_MINIMUM_WIDTH: f32 = 180.0;

pub(crate) fn initialize_controls(cx: &mut App) -> gpui::Result<()> {
    appearance::initialize(cx);
    let installed = cx.global::<appearance::InstalledChrome>();
    let motion = appearance_runtime::control_motion(cx);
    let active = Box::new(control_theme::catalog(&installed.active, motion));
    let inactive = Box::new(control_theme::catalog(&installed.inactive, motion));
    let settings = cx.global::<appearance::settings::InstalledSettingsChrome>();
    let settings_active = Box::new(control_theme::catalog(&settings.active.chrome, motion));
    let settings_inactive = Box::new(control_theme::catalog(&settings.inactive.chrome, motion));
    spaceterm_ui::init(cx, active, inactive, settings_active, settings_inactive)?;
    Ok(())
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
    cx.set_global(
        crate::platform::window_frame::WindowFrameGeometry::new(Some(16.0))
            .with_outer_edge_width(1.0),
    );
    cx.set_global(crate::platform::window_chrome::WindowChrome::native(None));
    initialize_controls(cx)?;
    let keymap = crate::desktop_profile::testing_profile(direction).install(cx);
    crate::keybindings::runtime::install(keymap, cx);
    gpui::BorrowAppContext::update_global::<crate::desktop_profile::DesktopPresentation, _>(
        cx,
        |presentation, cx| {
            presentation.refresh(cx);
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use gpui::{Action, Keystroke, TestAppContext};

    use super::*;

    #[gpui::test]
    fn ui_init_should_install_control_themes(cx: &mut TestAppContext) {
        cx.update(|cx| {
            init_with_text_direction(cx, spaceterm_ui::TextDirection::LeftToRight)
                .expect("UI initialization should succeed")
        });

        assert!(cx.update(|cx| {
            let floating = &appearance::chrome(cx).floating_colors;
            cx.has_global::<spaceterm_ui::ButtonTheme>()
                && cx.has_global::<spaceterm_ui::ToggleTheme>()
                && cx.has_global::<spaceterm_ui::ProgressTheme>()
                && cx.has_global::<spaceterm_ui::ScrollbarTheme>()
                && cx.has_global::<spaceterm_ui::ResizeHandleTheme>()
                && cx.has_global::<spaceterm_ui::SearchFieldTheme>()
                && cx.has_global::<spaceterm_ui::MenuTheme>()
                && cx.has_global::<spaceterm_ui::CommandPaletteTheme>()
                && cx.has_global::<spaceterm_ui::ComboBoxTheme>()
                && cx.has_global::<spaceterm_ui::TextInputTheme>()
                && cx.has_global::<spaceterm_ui::TooltipTheme>()
                && cx.has_global::<spaceterm_ui::ModalTheme>()
                && *cx.global::<spaceterm_ui::ProgressTheme>()
                    == control_theme::progress::theme(&appearance::chrome(cx).colors)
                && *cx.global::<spaceterm_ui::ControlMotion>()
                    == spaceterm_ui::ControlMotion::Standard
                && *cx.global::<spaceterm_ui::ModalTheme>() == control_theme::modal::theme(floating)
                && cx.has_global::<spaceterm_ui::ModalDesktopPolicy>()
                && *cx.global::<spaceterm_ui::ModalDesktopPolicy>()
                    == spaceterm_ui::ModalDesktopPolicy::mac_os()
        }));
    }

    #[gpui::test]
    fn ui_init_should_install_explicit_modal_command_period_binding(cx: &mut TestAppContext) {
        cx.update(|cx| init(cx).expect("UI initialization should succeed"));
        let command_period = Keystroke::parse("cmd-.").expect("macOS modal Shortcut should parse");

        let has_binding = cx.update(|cx| {
            cx.all_bindings_for_input(&[command_period])
                .iter()
                .any(|binding| {
                    binding.action().name() == "spaceterm_modal::ActivatePlatformCancel"
                        && binding.predicate().is_some_and(|predicate| {
                            predicate.eval(&[gpui::KeyContext::parse("SpaceTermModal").unwrap()])
                                && !predicate.eval(&[gpui::KeyContext::parse("Terminal").unwrap()])
                        })
                })
        });

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
    fn terminal_find_shortcuts_should_bind_supplied_desktop_actions(cx: &mut TestAppContext) {
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
            ("cmd-shift-k", SwitchWorkspace.name()),
            ("cmd-n", NewWorkspace.name()),
            ("cmd-shift-n", NewRemoteWorkspace.name()),
            ("cmd-o", OpenLocalDirectory.name()),
            ("cmd-shift-o", OpenRemoteDirectory.name()),
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
                    let binding = bindings.last().expect("the command has a binding");
                    assert!(binding.predicate().is_none(), "{shortcut} must be global");
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
