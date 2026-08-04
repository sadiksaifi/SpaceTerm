use std::path::PathBuf;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size,
};

use crate::terminal::{NativeTerminalSessionFactory, TerminalSessionFactory};
use crate::ui::WorkspaceManager;

pub(crate) fn open(cx: &mut App) {
    let Some(home_directory) = std::env::var_os("HOME").map(PathBuf::from) else {
        eprintln!("failed to open Termspace because the user home directory is unavailable");
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
        eprintln!("failed to open Termspace window: {error:#}");
        cx.quit();
        return;
    }

    cx.activate(true);
}
