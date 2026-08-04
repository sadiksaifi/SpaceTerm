mod app;
mod domain;
mod platform;
mod terminal;
mod theme;
mod ui;

use gpui::{App, Application};

fn main() {
    Application::new().run(|cx: &mut App| {
        ui::init(cx);
        app::open(cx);
    });
}
