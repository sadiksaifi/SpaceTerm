use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::terminal::{NativeTerminalSessionFactory, TerminalSessionFactory};
use crate::ui::{
    ClosePane, CloseWindow, CloseWorkspace, CopySelection, CreateWindow, CreateWorkspace,
    ExportTerminalDiagnostics, FindNext, FindPrevious, OpenLocalProject, OpenTerminalFind,
    WorkspaceManager,
};

actions!(
    spaceterm,
    [
        QuitApplication,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
        MinimizeWindow,
        ToggleFullScreen,
        CopyTerminalSelection
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
        KeyBinding::new("cmd-c", CopyTerminalSelection, None),
    ]);
    cx.on_action(|_: &QuitApplication, cx| cx.quit());
    cx.on_action(|_: &HideApplication, cx| cx.hide());
    cx.on_action(|_: &HideOtherApplications, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAllApplications, cx| cx.unhide_other_apps());
    cx.on_action(minimize_active_window);
    cx.on_action(toggle_active_window_full_screen);
    cx.on_action(|_: &CopyTerminalSelection, cx| {
        cx.defer(|cx| cx.dispatch_action(&CopySelection));
    });
    cx.on_app_quit(|_| {
        if let Err(error) = crate::platform::acceptance_observation::finish_runtime_observation() {
            eprintln!("acceptance runtime observation did not complete: {error}");
        }
        async {}
    })
    .detach();
    cx.set_menus(default_menus());
}

fn default_menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New Workspace", CreateWorkspace),
                MenuItem::action("Open Local Project…", OpenLocalProject),
                MenuItem::action("New Window", CreateWindow),
                MenuItem::separator(),
                MenuItem::action("Close Pane", ClosePane),
                MenuItem::action("Close Window", CloseWindow),
                MenuItem::action("Close Workspace", CloseWorkspace),
                MenuItem::separator(),
                MenuItem::action("Export Terminal Diagnostics…", ExportTerminalDiagnostics),
            ],
        },
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
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::action("Copy", CopyTerminalSelection),
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
    ]
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
    use gpui::{Action, ClipboardItem, Keystroke, TestAppContext};

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
            ("cmd-c", CopyTerminalSelection.name()),
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

    #[test]
    fn default_menus_should_present_the_standard_file_menu() {
        let menus = default_menus();
        let names = menus
            .iter()
            .map(|menu| menu.name.to_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, ["File", "SpaceTerm", "Edit", "Window"]);

        let file_items = menus[0]
            .items
            .iter()
            .map(|item| match item {
                MenuItem::Action { name, action, .. } => {
                    Some((name.to_string(), action.name().to_owned()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let expected_file_items = vec![
            Some((
                "New Workspace".to_owned(),
                CreateWorkspace.name().to_owned(),
            )),
            Some((
                "Open Local Project…".to_owned(),
                OpenLocalProject.name().to_owned(),
            )),
            Some(("New Window".to_owned(), CreateWindow.name().to_owned())),
            None,
            Some(("Close Pane".to_owned(), ClosePane.name().to_owned())),
            Some(("Close Window".to_owned(), CloseWindow.name().to_owned())),
            Some((
                "Close Workspace".to_owned(),
                CloseWorkspace.name().to_owned(),
            )),
            None,
            Some((
                "Export Terminal Diagnostics…".to_owned(),
                ExportTerminalDiagnostics.name().to_owned(),
            )),
        ];
        assert_eq!(file_items, expected_file_items);
        assert!(matches!(menus[0].items[3], MenuItem::Separator));
        assert!(matches!(menus[0].items[7], MenuItem::Separator));

        let export_is_absent_from_the_application_menu = !menus[1].items.iter().any(|item| {
            matches!(item, MenuItem::Action { name, .. } if name.as_ref() == "Export Terminal Diagnostics…")
        });
        assert!(export_is_absent_from_the_application_menu);
    }

    #[gpui::test]
    fn file_menu_shortcuts_should_remain_bound_to_their_actions(cx: &mut TestAppContext) {
        cx.update(crate::ui::init);
        cx.update(init);
        let expected = [
            ("cmd-n", CreateWorkspace.name()),
            ("cmd-o", OpenLocalProject.name()),
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
