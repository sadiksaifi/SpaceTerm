mod app;
mod domain;
mod platform;
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
