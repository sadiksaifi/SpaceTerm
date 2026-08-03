use gpui::prelude::*;
use gpui::{App, Application, Context, IntoElement, Render, Window, WindowOptions, div};

pub struct HelloWorld;

impl Render for HelloWorld {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child("Hello, World!")
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        if let Err(error) = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| HelloWorld))
        {
            eprintln!("failed to open Termspace window: {error:#}");
            cx.quit();
            return;
        }

        cx.activate(true);
    });
}
