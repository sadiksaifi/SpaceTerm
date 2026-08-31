mod app;
mod domain;
mod platform;
mod ssh;
mod terminal;
mod theme;
mod ui;

use gpui::{App, Application};

fn main() {
    if let Err(error) = platform::acceptance_observation::configure_from_environment() {
        eprintln!("failed to configure acceptance observation: {error}");
        std::process::exit(2);
    }
    Application::new()
        .with_assets(spaceterm_ui::ControlAssets)
        .run(|cx: &mut App| {
            ui::init(cx);
            app::init(cx);
            app::open(cx);
        });
    if let Err(error) = platform::acceptance_observation::finish_runtime_observation() {
        eprintln!("acceptance runtime observation did not complete: {error}");
    }
}
