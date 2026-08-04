use std::rc::Rc;

use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

use crate::terminal::{NativeTerminalSessionFactory, TerminalSessionFactory};
use crate::ui::WindowManager;

pub(crate) fn open(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    let session_factory: Rc<dyn TerminalSessionFactory> = Rc::new(NativeTerminalSessionFactory);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(420.0), px(260.0))),
            ..WindowOptions::default()
        },
        |window, cx| {
            window.set_window_title("Termspace");
            let window_manager =
                cx.new(|cx| WindowManager::new(Rc::clone(&session_factory), window, cx));
            window_manager.update(cx, |window_manager, cx| {
                window_manager.focus(window, cx);
            });
            window_manager
        },
    );

    if let Err(error) = result {
        eprintln!("failed to open Termspace window: {error:#}");
        cx.quit();
        return;
    }

    cx.activate(true);
}
