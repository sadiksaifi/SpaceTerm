use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, px, size,
};
use spaceterm_ui::{EditCopy, EditCut, EditPaste, EditRedo, EditSelectAll, EditUndo};

use crate::terminal::{NativeTerminalSessionFactory, TerminalSessionFactory};
use crate::ui::{
    ClosePane, CloseWindow, CloseWorkspace, CreateScratchWorkspace, CreateWindow,
    ExportTerminalDiagnostics, FindNext, FindPrevious, OpenLocalProject, OpenTerminalFind,
    SearchWorkspaces, ShowNewWorkspacePanel, WorkspaceManager,
};

actions!(
    spaceterm,
    [
        QuitApplication,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ToggleFullScreen
    ]
);

pub(crate) fn init(cx: &mut App) {
    #[cfg(not(test))]
    if let Err(error) = crate::platform::macos_services::register() {
        eprintln!("failed to register macOS Services types: {error}");
    }
    cx.bind_keys([
        KeyBinding::new("cmd-q", QuitApplication, None),
        KeyBinding::new("cmd-h", HideApplication, None),
        KeyBinding::new("alt-cmd-h", HideOtherApplications, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("fn-f", ToggleFullScreen, None),
    ]);
    cx.on_action(|_: &QuitApplication, cx| cx.quit());
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(minimize_active_window);
    cx.on_action(toggle_active_window_full_screen);
    cx.on_app_quit(|_| {
        if let Err(error) = crate::platform::acceptance_observation::finish_runtime_observation() {
            eprintln!("acceptance runtime observation did not complete: {error}");
        }
        async {}
    })
    .detach();
    cx.set_menus(vec![
        Menu {
            name: "SpaceTerm".into(),
            items: vec![
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide SpaceTerm", HideApplication),
                MenuItem::action("Hide Others", HideOtherApplications),
                MenuItem::action("Show All", ShowAllApplications),
                MenuItem::separator(),
                MenuItem::action("Quit SpaceTerm", QuitApplication),
            ],
        },
        file_menu(),
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Undo", EditUndo),
                MenuItem::action("Redo", EditRedo),
                MenuItem::separator(),
                MenuItem::action("Cut", EditCut),
                MenuItem::action("Copy", EditCopy),
                MenuItem::action("Paste", EditPaste),
                MenuItem::action("Select All", EditSelectAll),
                MenuItem::separator(),
                MenuItem::submenu(Menu {
                    name: "Find".into(),
                    items: vec![
                        MenuItem::action("Find…", OpenTerminalFind),
                        MenuItem::action("Find Next", FindNext),
                        MenuItem::action("Find Previous", FindPrevious),
                    ],
                }),
            ],
        },
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", MinimizeWindow),
                MenuItem::action("Toggle Full Screen", ToggleFullScreen),
            ],
        },
    ]);
}

fn file_menu() -> Menu {
    Menu {
        name: "File".into(),
        items: vec![
            MenuItem::action("New Workspace…", ShowNewWorkspacePanel),
            MenuItem::action("New Scratch Workspace", CreateScratchWorkspace),
            MenuItem::action("Open Local Project…", OpenLocalProject),
            MenuItem::action("Search Workspaces…", SearchWorkspaces),
            MenuItem::action("New Window", CreateWindow),
            MenuItem::separator(),
            MenuItem::action("Close Pane", ClosePane),
            MenuItem::action("Close Window", CloseWindow),
            MenuItem::action("Close Workspace", CloseWorkspace),
            MenuItem::separator(),
            MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
        ],
    }
}

fn minimize_active_window(_: &MinimizeWindow, cx: &mut App) {
    let Some(active_window) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        if let Err(error) = active_window.update(cx, |_, window, _| window.minimize_window()) {
            eprintln!("failed to minimize the active SpaceTerm window: {error:#}");
        }
    });
}

fn toggle_active_window_full_screen(_: &ToggleFullScreen, cx: &mut App) {
    let Some(active_window) = cx.active_window() else {
        return;
    };
    cx.defer(move |cx| {
        if let Err(error) = active_window.update(cx, |_, window, _| window.toggle_fullscreen()) {
            eprintln!("failed to toggle full screen for the active SpaceTerm window: {error:#}");
        }
    });
}

pub(crate) fn open(cx: &mut App) {
    let Some(home_directory) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("failed to open SpaceTerm because the user home directory is unavailable");
        cx.quit();
        return;
    };
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(NativeTerminalSessionFactory);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(480.0), px(260.0))),
            titlebar: Some(TitlebarOptions {
                title: None,
                appears_transparent: true,
                traffic_light_position: Some(point(px(12.0), px(11.0))),
            }),
            ..WindowOptions::default()
        },
        |window, cx| {
            let workspace_manager = cx.new(|cx| {
                WorkspaceManager::new(
                    Rc::clone(&session_factory),
                    home_directory.clone(),
                    window,
                    cx,
                )
            });
            workspace_manager.update(cx, |workspace_manager, cx| {
                workspace_manager.focus(window, cx);
            });
            if let Err(error) = crate::platform::macos_services::install(window, cx) {
                eprintln!("failed to install the macOS Services responder: {error}");
            }
            workspace_manager
        },
    );

    if let Err(error) = result {
        eprintln!("failed to open SpaceTerm window: {error:#}");
        cx.quit();
        return;
    }

    cx.activate(true);
}

#[cfg(test)]
mod tests {
    use gpui::{Action, ClipboardItem, Keystroke, OwnedMenuItem, TestAppContext};

    use super::*;
    use crate::terminal::testing::{TestTerminalSessionFactory, TestTerminalSessionRecords};
    use crate::terminal::{SelectionCopy, WorkspaceTerminalSessionFactory};
    use crate::ui::TerminalPane;

    #[gpui::test]
    fn standard_macos_shortcuts_should_bind_global_application_actions(cx: &mut TestAppContext) {
        cx.update(init);
        let expected = [
            ("cmd-q", QuitApplication.name()),
            ("cmd-h", HideApplication.name()),
            ("alt-cmd-h", HideOtherApplications.name()),
            ("cmd-m", MinimizeWindow.name()),
            ("ctrl-cmd-f", ToggleFullScreen.name()),
            ("fn-f", ToggleFullScreen.name()),
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
    fn file_menu_should_use_the_workspace_directory_action_order(cx: &mut TestAppContext) {
        let labels = cx.update(|_| {
            let file = file_menu().owned();
            file.items
                .iter()
                .map(|item| match item {
                    OwnedMenuItem::Action { name, .. } => name.clone(),
                    OwnedMenuItem::Separator => "|".to_owned(),
                    OwnedMenuItem::Submenu(_) | OwnedMenuItem::SystemMenu(_) => {
                        "submenu".to_owned()
                    }
                })
                .collect::<Vec<_>>()
        });

        assert_eq!(
            labels,
            vec![
                "New Workspace…",
                "New Scratch Workspace",
                "Open Local Project…",
                "Search Workspaces…",
                "New Window",
                "|",
                "Close Pane",
                "Close Window",
                "Close Workspace",
                "|",
                "Export Terminal Diagnostics…",
            ]
        );
    }

    #[gpui::test]
    fn native_copy_command_dispatches_semantic_copy_to_the_terminal(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        cx.update(init);
        let records = TestTerminalSessionRecords::default();
        let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(
            TestTerminalSessionFactory::new(records.clone()).with_selection_copy_response(Ok(
                Some(SelectionCopy {
                    plain_text: "native command copy".to_owned(),
                    html: None,
                }),
            )),
        );
        let session_factory = WorkspaceTerminalSessionFactory::new(
            session_factory,
            PathBuf::from("/tmp/spaceterm-native-copy-command-test"),
        );
        let (pane, cx) =
            cx.add_window_view(|window, cx| TerminalPane::new(session_factory, window, cx));
        cx.update(|window, cx| {
            window.activate_window();
            pane.update(cx, |pane, _| pane.focus(window));
        });
        cx.run_until_parked();
        cx.write_to_clipboard(ClipboardItem::new_string("stale clipboard".to_owned()));

        cx.simulate_keystrokes("cmd-c");
        cx.run_until_parked();

        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("native command copy".to_owned())
        );
    }
}
