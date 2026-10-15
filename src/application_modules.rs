#[cfg(feature = "desktop")]
mod about;
#[cfg(feature = "desktop")]
mod app;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod appearance;
#[cfg(feature = "desktop")]
mod application_identity;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod background_image;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod bundled_font;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod close_confirmation;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop policy shares types with the headless runtime"
    )
)]
mod domain;
#[cfg(feature = "desktop")]
mod host_fonts;
#[cfg(feature = "desktop")]
mod keybindings;
mod platform;
#[cfg(feature = "desktop")]
mod repository_status;
#[cfg(feature = "desktop")]
mod settings;
#[cfg_attr(
    not(feature = "desktop"),
    allow(
        dead_code,
        unused_imports,
        reason = "desktop SSH orchestration shares launch-plan types"
    )
)]
mod ssh;
mod terminal;
#[cfg(feature = "desktop")]
mod theme_registry;
#[cfg(feature = "desktop")]
mod ui;
#[cfg(feature = "desktop")]
mod updates;
#[cfg(feature = "desktop")]
mod worktrees;

#[cfg(feature = "desktop")]
mod desktop_profile;

#[cfg(test)]
#[cfg(feature = "desktop")]
mod architecture_tests;

#[cfg(feature = "desktop")]
mod directory_selection;
mod local_path;

/// Terminal runtime contracts shared by graphical and headless hosts.
pub mod runtime;
