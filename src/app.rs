use gpui::{App, AppContext, Bounds, WindowBounds, WindowOptions, px, size};

use crate::ui::TerminalPane;

pub(crate) fn open(cx: &mut App) {
    let bounds = Bounds::centered(None, size(px(900.0), px(580.0)), cx);
    let result = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(420.0), px(260.0))),
            ..WindowOptions::default()
        },
        |window, cx| {
            window.set_window_title("Termspace");
            let pane = cx.new(|cx| TerminalPane::new(window, cx));
            pane.update(cx, |pane, _cx| pane.focus(window));
            pane
        },
    );

    if let Err(error) = result {
        eprintln!("failed to open Termspace window: {error:#}");
        cx.quit();
        return;
    }

    cx.activate(true);
}
