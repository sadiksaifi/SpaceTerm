mod about;
mod app;
mod appearance;
mod application_identity;
mod background_image;
mod bundled_font;
mod close_confirmation;
mod domain;
mod host_fonts;
mod keybindings;
mod platform;
mod repository_status;
mod settings;
mod ssh;
mod terminal;
mod theme_registry;
mod ui;
mod updates;
mod worktrees;

mod desktop_profile;

#[cfg(test)]
mod architecture_tests;

#[cfg(test)]
#[path = "../crates/spaceterm-host/tests/support/signed_installation.rs"]
mod signed_installation_fixture;

mod directory_selection;
mod local_path;
