mod app;
mod appearance;
mod close_confirmation;
mod domain;
mod platform;
mod settings;
mod ssh;
mod terminal;
mod theme;
mod ui;

mod desktop_profile;

fn main() {
    platform::main();
}

#[cfg(test)]
mod architecture_tests;

mod directory_selection;
mod local_path;
