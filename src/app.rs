use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType, TitlebarOptions,
    WindowBounds, WindowOptions, actions, point, px, size,
};

use crate::terminal::{NativeTerminalSessionFactory, TerminalSessionFactory};
use crate::ui::WorkspaceManager;

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
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", MinimizeWindow),
                MenuItem::action("Toggle Full Screen", ToggleFullScreen),
            ],
        },
    ]);
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
    use gpui::{Action, Keystroke, TestAppContext};

    use super::*;

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
}
